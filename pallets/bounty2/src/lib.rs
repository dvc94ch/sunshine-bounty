#![allow(clippy::string_lit_as_bytes)]
#![allow(clippy::redundant_closure_call)]
#![allow(clippy::type_complexity)]
#![cfg_attr(not(feature = "std"), no_std)]
//! Bounty pallet with refundable contributions and more contributor voting rights

use codec::Codec;
use frame_support::{
    decl_error,
    decl_event,
    decl_module,
    decl_storage,
    ensure,
    storage::IterableStorageMap,
    traits::{
        Currency,
        ExistenceRequirement,
        Get,
        ReservableCurrency,
        WithdrawReason,
        WithdrawReasons,
    },
    Parameter,
};
use frame_system::ensure_signed;
use sp_runtime::{
    traits::{
        AccountIdConversion,
        AtLeast32Bit,
        MaybeSerializeDeserialize,
        Member,
        Zero,
    },
    DispatchResult,
    ModuleId,
    Permill,
};
use sp_std::{
    fmt::Debug,
    prelude::*,
};
use util::{
    bounty::{
        BountySubmission,
        SubmissionState,
    },
    grant::{
        ChallengeNorms,
        Foundation,
    },
};

// type aliases
type BalanceOf<T> = <<T as Trait>::Currency as Currency<
    <T as frame_system::Trait>::AccountId,
>>::Balance;
type Bounty<T> = Foundation<
    <T as Trait>::IpfsReference,
    BalanceOf<T>,
    ChallengeNorms<<T as frame_system::Trait>::AccountId, Permill>,
>;
type BountySub<T> = BountySubmission<
    <T as Trait>::BountyId,
    <T as Trait>::IpfsReference,
    <T as frame_system::Trait>::AccountId,
    BalanceOf<T>,
    SubmissionState,
>;

pub trait Trait: frame_system::Trait {
    /// The overarching event type
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;

    /// Cid type
    type IpfsReference: Parameter + Member + Default;

    /// The currency type
    type Currency: Currency<Self::AccountId>
        + ReservableCurrency<Self::AccountId>;

    /// The bounty post identifier
    type BountyId: Parameter
        + Member
        + AtLeast32Bit
        + Codec
        + Default
        + Copy
        + MaybeSerializeDeserialize
        + Debug
        + PartialOrd
        + PartialEq
        + Zero;

    /// The bounty submission identifier
    type SubmissionId: Parameter
        + Member
        + AtLeast32Bit
        + Codec
        + Default
        + Copy
        + MaybeSerializeDeserialize
        + Debug
        + PartialOrd
        + PartialEq
        + Zero;

    /// Period for which every spend decision can be challenged with a veto and/or refund request
    type ChallengePeriod: Get<Self::BlockNumber>;

    /// Frequency at which submissions are polled in on_finalize
    type SubmissionPollFrequency: Get<Self::BlockNumber>;

    /// The foundational foundation
    type Foundation: Get<ModuleId>;

    /// Minimum deposit to post bounty
    type MinDeposit: Get<BalanceOf<Self>>;

    /// Minimum contribution to posted bounty
    type MinContribution: Get<BalanceOf<Self>>;

    /// Minimum veto threshold
    type MinVetoThreshold: Get<Permill>;

    /// Minimum refund threshold
    type MinRefundThreshold: Get<Permill>;
}

decl_event!(
    pub enum Event<T>
    where
        <T as frame_system::Trait>::AccountId,
        <T as Trait>::IpfsReference,
        <T as Trait>::BountyId,
        <T as Trait>::SubmissionId,
        Balance = BalanceOf<T>,
    {
        /// Poster, Initial Amount, Identifier, Bounty Metadata (i.e. github issue reference)
        BountyPosted(AccountId, Balance, BountyId, IpfsReference),
        /// Contributor, This Contribution Amount, Identifier, Full Amount After Contribution, Bounty Metadata
        BountyRaiseContribution(AccountId, Balance, BountyId, Balance, IpfsReference),
        /// Submitter, Bounty Identifier, Amount Requested, Submission Identifier, Bounty Metadata, Submission Metadata
        BountySubmissionPosted(AccountId, BountyId, Balance, SubmissionId, IpfsReference, IpfsReference),
        /// Bounty Identifier, Full Amount Left After Payment, Submission Identifier, Amount Requested, Bounty Metadata, Submission Metadata
        BountyPaymentExecuted(BountyId, Balance, SubmissionId, Balance, AccountId, IpfsReference, IpfsReference),
    }
);

decl_error! {
    pub enum Error for Module<T: Trait> {
        // Bounty Does Not Exist
        BountyDNE,
        SubmissionDNE,
        BountyPostMustExceedMinDeposit,
        ContributionMustExceedModuleMin,
        DepositerCannotSubmitForBounty,
        BountySubmissionExceedsTotalAvailableFunding,
        SubmissionNotInValidStateToApprove,
        CannotApproveSubmissionIfAmountExceedsTotalAvailable,
        NotAuthorizedToApproveBountySubmissions,
    }
}

decl_storage! {
    trait Store for Module<T: Trait> as Bounty {
        /// Uid generation helper for BountyId
        BountyNonce get(fn bounty_nonce): T::BountyId;

        /// Uid generation helpers for SubmissionId
        SubmissionNonce get(fn submission_nonce): T::SubmissionId;

        /// Posted Bounties
        pub Bounties get(fn bounties): map
            hasher(blake2_128_concat) T::BountyId => Option<Bounty<T>>;
        /// Tips for existing Bounties
        pub BountyTips get(fn bounty_tips): double_map
            hasher(blake2_128_concat) T::BountyId,
            hasher(blake2_128_concat) T::AccountId => Option<BalanceOf<T>>;

        /// Posted Submissions
        pub Submissions get(fn submissions): map
            hasher(blake2_128_concat) T::SubmissionId => Option<BountySub<T>>;
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        type Error = Error<T>;
        fn deposit_event() = default;

        #[weight = 0]
        fn post_bounty(
            origin,
            info: T::IpfsReference,
            amount: BalanceOf<T>,
            veto_threshold: Permill,
            refund_threshold: Permill,
        ) -> DispatchResult {
            let depositer = ensure_signed(origin)?;
            ensure!(amount >= T::MinDeposit::get(), Error::<T>::BountyPostMustExceedMinDeposit);
            let imb = T::Currency::withdraw(
                &depositer,
                amount,
                WithdrawReasons::from(WithdrawReason::Transfer),
                ExistenceRequirement::AllowDeath,
            )?;
            let bounty = Bounty::<T>::new(info.clone(), amount, ChallengeNorms::new(depositer.clone(), veto_threshold, refund_threshold));
            let id = Self::bounty_generate_uid();
            T::Currency::resolve_creating(&Self::bounty_account_id(id), imb);
            <Bounties<T>>::insert(id, bounty);
            <BountyTips<T>>::insert(id, &depositer, amount);
            Self::deposit_event(RawEvent::BountyPosted(depositer, amount, id, info));
            Ok(())
        }
        #[weight = 0]
        fn contribute_to_bounty(
            origin,
            bounty_id: T::BountyId,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let contributor = ensure_signed(origin)?;
            ensure!(amount >= T::MinContribution::get(), Error::<T>::ContributionMustExceedModuleMin);
            let bounty = <Bounties<T>>::get(bounty_id).ok_or(Error::<T>::BountyDNE)?;
            T::Currency::transfer(
                &contributor,
                &Self::bounty_account_id(bounty_id),
                amount,
                ExistenceRequirement::KeepAlive,
            )?;
            let new_amount = if let Some(a) = <BountyTips<T>>::get(bounty_id, &contributor) {
                amount + a
            } else {
                amount
            };
            let new_bounty = bounty.add_funds(amount);
            let total = new_bounty.funds();
            <BountyTips<T>>::insert(bounty_id, &contributor, new_amount);
            <Bounties<T>>::insert(bounty_id, new_bounty);
            Self::deposit_event(RawEvent::BountyRaiseContribution(contributor, amount, bounty_id, total, bounty.info()));
            Ok(())
        }
        #[weight = 0]
        fn submit_for_bounty(
            origin,
            bounty_id: T::BountyId,
            submission_ref: T::IpfsReference,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let submitter = ensure_signed(origin)?;
            let bounty = <Bounties<T>>::get(bounty_id).ok_or(Error::<T>::BountyDNE)?;
            ensure!(submitter != bounty.gov().leader(), Error::<T>::DepositerCannotSubmitForBounty);
            ensure!(amount <= bounty.funds(), Error::<T>::BountySubmissionExceedsTotalAvailableFunding);
            let submission = BountySub::<T>::new(bounty_id, submission_ref.clone(), submitter.clone(), amount);
            let id = Self::submission_generate_uid();
            <Submissions<T>>::insert(id, submission);
            Self::deposit_event(RawEvent::BountySubmissionPosted(submitter, bounty_id, amount, id, bounty.info(), submission_ref));
            Ok(())
        }
        #[weight = 0]
        fn approve_bounty_submission(
            origin,
            submission_id: T::SubmissionId,
        ) -> DispatchResult {
            let approver = ensure_signed(origin)?;
            let submission = <Submissions<T>>::get(submission_id).ok_or(Error::<T>::SubmissionDNE)?;
            ensure!(submission.awaiting_review(), Error::<T>::SubmissionNotInValidStateToApprove);
            let bounty_id = submission.bounty_id();
            let bounty = <Bounties<T>>::get(bounty_id).ok_or(Error::<T>::BountyDNE)?;
            ensure!(bounty.funds() >= submission.amount(), Error::<T>::CannotApproveSubmissionIfAmountExceedsTotalAvailable);
            ensure!(bounty.gov().leader() == approver, Error::<T>::NotAuthorizedToApproveBountySubmissions);
            // execute payment
            T::Currency::transfer(
                &Self::bounty_account_id(bounty_id),
                &submission.submitter(),
                submission.amount(),
                ExistenceRequirement::KeepAlive,
            )?;
            let new_bounty = bounty.subtract_funds(submission.amount());
            let (bounty_info, new_total) = (new_bounty.info(), new_bounty.funds());
            // submission approved and executed => can be removed
            <Submissions<T>>::remove(submission_id);
            <Bounties<T>>::insert(bounty_id, new_bounty);
            Self::deposit_event(RawEvent::BountyPaymentExecuted(bounty_id, new_total, submission_id, submission.amount(), submission.submitter(), bounty_info, submission.submission()));
            Ok(())
        }
    }
}

// ID helpers
impl<T: Trait> Module<T> {
    pub fn bounty_account_id(index: T::BountyId) -> T::AccountId {
        T::Foundation::get().into_sub_account(index)
    }
    fn bounty_id_is_available(id: T::BountyId) -> bool {
        <Bounties<T>>::get(id).is_none()
    }
    fn bounty_generate_uid() -> T::BountyId {
        let mut id_counter = <BountyNonce<T>>::get() + 1u32.into();
        while !Self::bounty_id_is_available(id_counter) {
            id_counter += 1u32.into();
        }
        <BountyNonce<T>>::put(id_counter);
        id_counter
    }
    fn submission_id_is_available(id: T::SubmissionId) -> bool {
        <Submissions<T>>::get(id).is_none()
    }
    fn submission_generate_uid() -> T::SubmissionId {
        let mut id_counter = <SubmissionNonce<T>>::get() + 1u32.into();
        while !Self::submission_id_is_available(id_counter) {
            id_counter += 1u32.into();
        }
        <SubmissionNonce<T>>::put(id_counter);
        id_counter
    }
    fn _recursive_remove_bounty(id: T::BountyId) {
        <Bounties<T>>::remove(id);
        <Submissions<T>>::iter()
            .filter(|(_, app)| app.bounty_id() == id)
            .for_each(|(app_id, _)| <Submissions<T>>::remove(app_id));
    }
}

// Storage helpers
impl<T: Trait> Module<T> {
    pub fn open_bounties(
        min: BalanceOf<T>,
    ) -> Option<Vec<(T::BountyId, Bounty<T>)>> {
        let ret = <Bounties<T>>::iter()
            .filter(|(_, b)| b.funds() >= min)
            .map(|(id, bounty)| (id, bounty))
            .collect::<Vec<(T::BountyId, Bounty<T>)>>();
        if ret.is_empty() {
            None
        } else {
            Some(ret)
        }
    }
    pub fn open_submissions(
        bounty_id: T::BountyId,
    ) -> Option<Vec<(T::SubmissionId, BountySub<T>)>> {
        let ret = <Submissions<T>>::iter()
            .filter(|(_, s)| s.bounty_id() == bounty_id)
            .map(|(id, sub)| (id, sub))
            .collect::<Vec<(T::SubmissionId, BountySub<T>)>>();
        if ret.is_empty() {
            None
        } else {
            Some(ret)
        }
    }
}
