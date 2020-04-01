#![allow(clippy::string_lit_as_bytes)]
#![allow(clippy::redundant_closure_call)]
#![allow(clippy::type_complexity)]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

use codec::Codec;
use frame_support::{
    decl_error, decl_event, decl_module, decl_storage, ensure, traits::Get, Parameter,
};
use frame_system::{self as system, ensure_signed};
use sp_runtime::{
    traits::{AtLeast32Bit, MaybeSerializeDeserialize, Member},
    DispatchError, DispatchResult, Permill,
};
use sp_std::{fmt::Debug, prelude::*};
use util::{
    traits::{
        ApplyVote, Approved, CheckVoteStatus, GenerateUniqueID, GetMagnitude, GetVoteOutcome,
        GroupMembership, IDIsAvailable, LockableProfile, MintableSignal, OpenVote,
        ReservableProfile, ShareBank, ShareRegistration, VoteOnProposal, VoteThresholdBuilder,
    },
    vote::{Outcome, ThresholdConfig, VoteState, VoteThreshold, VoterYesNoView, YesNoVote},
};

/// The shares type that is converted into signal for each instance of this module
pub type SharesOf<T> =
    <<T as Trait>::ShareData as ShareRegistration<<T as frame_system::Trait>::AccountId>>::Shares;

/// The share identifier type
pub type ShareId<T> =
    <<T as Trait>::ShareData as ShareRegistration<<T as frame_system::Trait>::AccountId>>::ShareId;

/// The organization identifier type
pub type OrgId<T> =
    <<T as Trait>::ShareData as ShareRegistration<<T as frame_system::Trait>::AccountId>>::OrgId;

// TODO: make direct inheritance work
pub trait Trait: frame_system::Trait {
    type Event: From<Event<Self>> + Into<<Self as frame_system::Trait>::Event>;

    /// The identifier for each vote; ProposalId => Vec<VoteId> s.t. sum(VoteId::Outcomes) => ProposalId::Outcome
    type VoteId: Parameter
        + Member
        + AtLeast32Bit
        + Codec
        + Default
        + Copy
        + MaybeSerializeDeserialize
        + Debug;

    /// The native voting power type
    type Signal: Parameter
        + Member
        + AtLeast32Bit
        + Codec
        + Default
        + Copy
        + MaybeSerializeDeserialize
        + Debug
        + PartialOrd
        + From<SharesOf<Self>>
        + Into<SharesOf<Self>>;

    /// An instance of the shares module
    type ShareData: GroupMembership<Self::AccountId>
        + ShareRegistration<Self::AccountId>
        + ReservableProfile<Self::AccountId>
        + LockableProfile<Self::AccountId>
        + ShareBank<Self::AccountId>;

    /// The default vote length
    type DefaultVoteLength: Get<Self::BlockNumber>;
}

decl_event!(
    pub enum Event<T>
    where
        <T as frame_system::Trait>::AccountId,
        <T as Trait>::VoteId,
        ShareId = ShareId<T>,
        OrgId = OrgId<T>,
    {
        NewVoteStarted(OrgId, ShareId, VoteId),
        Voted(OrgId, ShareId, VoteId, AccountId),
    }
);

decl_error! {
    pub enum Error for Module<T: Trait> {
        VoteIdentityUnitialized,
        ShareMembershipUnitialized,
        CurrentBlockNumberNotMoreRecent,
        VoteStateUninitialized,
        /// This ensures that the outcome was initialized in order to allow `VoteOnProposal`
        VoteNotInitialized,
        /// This ensures that the user can only vote when the outcome is in voting
        CanOnlyVoteinVotingOutcome,
        VoterViewNotAccountedFor,
        /// Current time is past the time of the vote expiration so new votes are not accepted
        VotePastExpirationTimeSoVotesNotAccepted,
        NotEnoughSignalToVote,
        /// Tried to get voting outcome but returned None from storage
        NoOutcomeAssociatedWithVoteID,
    }
}

decl_storage! {
    trait Store for Module<T: Trait> as VoteYesNo {
        /// VoteId storage helper for unique id generation, see issue #62
        pub VoteIdCounter get(vote_id_counter): double_map
            hasher(blake2_256) OrgId<T>,
            hasher(blake2_256) ShareId<T>  => T::VoteId;

        /// Total signal available for each member for the vote in question
        pub MintedSignal get(minted_signal): double_map
            hasher(blake2_256) (OrgId<T>, ShareId<T>, T::VoteId),
            hasher(blake2_256) T::AccountId  => Option<T::Signal>;

        /// The state of a vote (separate from outcome so that this can be purged if Outcome is not Voting)
        pub VoteStates get(fn vote_states): double_map
            hasher(blake2_256) (OrgId<T>, ShareId<T>),
            hasher(blake2_256) T::VoteId => Option<VoteState<T::Signal, T::BlockNumber>>;

        /// The outcome of a vote
        pub VoteOutcome get(fn vote_outcome): double_map
            hasher(blake2_256) (OrgId<T>, ShareId<T>),
            hasher(blake2_256) T::VoteId => Option<Outcome>;
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        type Error = Error<T>;
        fn deposit_event() = default;

        const DefaultVoteLength: T::BlockNumber = T::DefaultVoteLength::get();

        fn create_vote(
            origin,
            organization: OrgId<T>,
            share_id: ShareId<T>,
            passage_threshold_pct: Permill,
            turnout_threshold_pct: Permill,
        ) -> DispatchResult {
            // TODO: replace with origin check once I give a shit about permissions
            let _ = ensure_signed(origin)?;
            let threshold_config = ThresholdConfig::new(passage_threshold_pct, turnout_threshold_pct);
            let new_vote_id = Self::open_vote(organization, share_id, None, threshold_config)?;
            // emit event
            Self::deposit_event(RawEvent::NewVoteStarted(organization, share_id, new_vote_id));
            Ok(())
        }

        fn submit_vote(
            origin,
            organization: OrgId<T>,
            share_id: ShareId<T>,
            vote_id: T::VoteId,
            voter: T::AccountId,
            direction: VoterYesNoView,
            magnitude: Option<T::Signal>
        ) -> DispatchResult {
            let _ = ensure_signed(origin)?;
            Self::vote_on_proposal(organization, share_id, vote_id, &voter, direction, magnitude)?;
            Self::deposit_event(RawEvent::Voted(organization, share_id, vote_id, voter));
            Ok(())
        }
    }
}

impl<T: Trait> IDIsAvailable<(OrgId<T>, ShareId<T>, T::VoteId)> for Module<T> {
    fn id_is_available(id: (OrgId<T>, ShareId<T>, T::VoteId)) -> bool {
        None == <VoteStates<T>>::get((id.0, id.1), id.2)
    }
}

impl<T: Trait> GenerateUniqueID<(OrgId<T>, ShareId<T>, T::VoteId)> for Module<T> {
    fn generate_unique_id(
        proposed_id: (OrgId<T>, ShareId<T>, T::VoteId),
    ) -> (OrgId<T>, ShareId<T>, T::VoteId) {
        if !Self::id_is_available(proposed_id) {
            let mut id_counter = <VoteIdCounter<T>>::get(proposed_id.0, proposed_id.1);
            while <VoteStates<T>>::get((proposed_id.0, proposed_id.1), id_counter).is_some() {
                // TODO: add overflow check here
                id_counter += 1.into();
            }
            <VoteIdCounter<T>>::insert(proposed_id.0, proposed_id.1, id_counter + 1.into());
            (proposed_id.0, proposed_id.1, id_counter)
        } else {
            proposed_id
        }
    }
}

impl<T: Trait> MintableSignal<OrgId<T>, ShareId<T>, T::AccountId, Permill> for Module<T> {
    /// Mints signal based on explicit mapping to share group share value, no multiplier
    fn mint_signal_based_on_existing_share_value(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: T::VoteId,
        who: &T::AccountId,
    ) -> Result<Self::Signal, DispatchError> {
        // Get the amount of shares reserved, we don't use times_reserved part of context here, for now
        let reservation_context = T::ShareData::reserve(organization, share_id, &who, None)?;
        let shares_reserved = reservation_context.get_magnitude();
        // could add more nuanced conversion logic here; see doc/sharetovote
        let minted_signal: T::Signal = shares_reserved.into();
        let prefix_key = (organization, share_id, vote_id);
        <MintedSignal<T>>::insert(prefix_key, who, minted_signal);
        Ok(minted_signal)
    }

    /// WARNING: CALL MUST BE PERMISSIONED
    ///
    /// Mints `amount` of Signal for `who`
    /// - overwrites any existing storage value without any checks, questionable?
    fn custom_mint_signal(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: T::VoteId,
        who: &T::AccountId,
        amount: Self::Signal,
    ) -> Result<Self::Signal, DispatchError> {
        let prefix_key = (organization, share_id, vote_id);
        <MintedSignal<T>>::insert(prefix_key, who, amount);
        Ok(amount)
    }

    /// This mints signal for all of the shareholders and reserves as many free shares as they have
    /// to do so. The cost scales with the size of the shareholder group (in number of AccountId members)
    /// because it mints for each share signal (by calling `mint_signal` with None for the amount parameter, to execute
    /// the default happy path of reserving as many shares as possible to mint the signal...)
    fn batch_mint_signal(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: Self::VoteId,
    ) -> Result<Self::Signal, DispatchError> {
        let new_vote_group = T::ShareData::shareholder_membership(organization, share_id)?;
        let mut total_minted_signal: T::Signal = 0.into();
        new_vote_group.iter().for_each(|who| {
            // does this propagate errors
            let minted_signal = Self::mint_signal_based_on_existing_share_value(
                organization,
                share_id,
                vote_id,
                who,
            );
            if let Ok(add_to_sum) = minted_signal {
                total_minted_signal += add_to_sum;
            }
            // TODO: PROPER ERROR HANDLING HERE
        });
        Ok(total_minted_signal)
    }
}

impl<T: Trait> VoteThresholdBuilder<Permill> for Module<T> {
    type Signal = T::Signal;
    type ThresholdConfig = ThresholdConfig<Permill>;
    type VoteThreshold = VoteThreshold<T::Signal, T::BlockNumber>;

    /// Essential From<ThresholdConfig> for VoteThreshold in the context of this module (runtime)
    fn build_vote_threshold(
        threshold_config: Self::ThresholdConfig,
        possible_turnout: Self::Signal,
    ) -> Self::VoteThreshold {
        // TODO: should add trait bound to ensure that these fields can multiply the possible turnout like this
        let support_required = threshold_config.passage_threshold_pct * possible_turnout;
        let turnout_required = threshold_config.turnout_threshold_pct * possible_turnout;
        let now = system::Module::<T>::block_number();
        Self::VoteThreshold::new(support_required, turnout_required, now)
    }
}

impl<T: Trait> GetVoteOutcome<OrgId<T>, ShareId<T>> for Module<T> {
    type VoteId = T::VoteId;
    type Outcome = Outcome;
    fn get_vote_outcome(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: Self::VoteId,
    ) -> Result<Self::Outcome, DispatchError> {
        let prefix_key = (organization, share_id);
        if let Some(outcome) = <VoteOutcome<T>>::get(prefix_key, vote_id) {
            Ok(outcome)
        } else {
            Err(Error::<T>::NoOutcomeAssociatedWithVoteID.into())
        }
    }
}

impl<T: Trait> OpenVote<OrgId<T>, ShareId<T>, T::AccountId, Permill> for Module<T> {
    fn open_vote(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: Option<Self::VoteId>,
        threshold_config: Self::ThresholdConfig,
    ) -> Result<Self::VoteId, DispatchError> {
        // TODO: generate uuid here for voteid especially if None
        // get a counter from the organization and iterate, checked add?
        let generated_vote_id: T::VoteId = if let Some(id) = vote_id {
            id
        } else {
            <VoteIdCounter<T>>::get(organization, share_id) + 1.into()
        };
        let proposed_joint_id = (organization, share_id, generated_vote_id);
        let new_joint_id = Self::generate_unique_id(proposed_joint_id);
        let new_vote_id = new_joint_id.2;

        // calculate `initialized` and `expires` fields for vote state
        let now = system::Module::<T>::block_number();
        let ends = now + T::DefaultVoteLength::get();
        // mint signal for all of shareholders and get total possible turnout
        let total_possible_turnout = Self::batch_mint_signal(organization, share_id, new_vote_id)?;
        // calculate the vote threshold from the threshold configuration passed in
        let threshold = Self::build_vote_threshold(threshold_config, total_possible_turnout);
        // TODO: replace this with a new method
        let new_vote_state = VoteState {
            // in_favor, against, and turnout are 0 by default
            threshold,
            initialized: now,
            expires: ends,
            ..Default::default()
        };

        // insert the VoteState
        let prefix_key = (organization, share_id);
        <VoteStates<T>>::insert(prefix_key, new_vote_id, new_vote_state);
        // insert the current VoteOutcome (voting)
        <VoteOutcome<T>>::insert(prefix_key, new_vote_id, Outcome::Voting);

        Ok(new_vote_id)
    }
}

impl<T: Trait> ApplyVote for Module<T> {
    type Vote = YesNoVote<T::Signal>;
    type State = VoteState<T::Signal, T::BlockNumber>;

    fn apply_vote(state: Self::State, vote: Self::Vote) -> Result<Self::State, DispatchError> {
        // update VoterStatus (which should wrap the vote for certain options)
        let new_state = match vote.direction {
            VoterYesNoView::InFavor => {
                // TODO: checked adds
                let new_in_favor = state.in_favor + vote.magnitude;
                let new_turnout = state.turnout + vote.magnitude;
                Self::State {
                    in_favor: new_in_favor,
                    turnout: new_turnout,
                    ..state
                }
            }
            VoterYesNoView::Against => {
                // TODO: checked adds
                let new_against = state.against + vote.magnitude;
                let new_turnout = state.turnout + vote.magnitude;
                Self::State {
                    against: new_against,
                    turnout: new_turnout,
                    ..state
                }
            }
            VoterYesNoView::Abstained => {
                // TODO: checked adds
                let new_turnout = state.turnout + vote.magnitude;
                Self::State {
                    turnout: new_turnout,
                    ..state
                }
            }
            _ => return Err(Error::<T>::VoterViewNotAccountedFor.into()),
        };
        Ok(new_state)
    }
}

// TODO => if approved, close the vote (and this logic should be associated with outcome)
impl<T: Trait> CheckVoteStatus for Module<T> {
    type Outcome = Outcome;

    // TO SEE IF THE OUTCOME HAS CHANGED
    fn check_vote_outcome(state: Self::State) -> Result<Self::Outcome, DispatchError> {
        // trait bound on Self::State ensures that it implements Approved so that's all we have for now
        if state.approved() {
            // this should trigger a state change in a different module (ie bank)
            // see https://substrate.dev/docs/en/tutorials/adding-a-module-to-your-runtime#adding-runtime-hooks
            return Ok(Outcome::Approved);
        }
        // TODO: add rejection outcome when we add the Rejected trait implementation for VoteState and bound to Self::State
        Ok(Outcome::Voting)
    }

    // TO SEE IF THE VOTE HAS EXPIRED
    fn check_vote_expired(state: Self::State) -> bool {
        let now = system::Module::<T>::block_number();
        state.expires < now
    }
}

impl<T: Trait> VoteOnProposal<OrgId<T>, ShareId<T>, T::AccountId, Permill> for Module<T> {
    type Direction = VoterYesNoView;
    type Magnitude = T::Signal;

    fn vote_on_proposal(
        organization: OrgId<T>,
        share_id: ShareId<T>,
        vote_id: Self::VoteId,
        voter: &T::AccountId,
        direction: Self::Direction,
        magnitude: Option<Self::Magnitude>,
    ) -> DispatchResult {
        // check that voting is permitted based on current outcome
        let first_prefix_key = (organization, share_id);
        let current_outcome = <VoteOutcome<T>>::get(first_prefix_key, vote_id)
            .ok_or(Error::<T>::VoteNotInitialized)?;
        ensure!(
            current_outcome == Outcome::Voting,
            Error::<T>::CanOnlyVoteinVotingOutcome
        );
        // get the vote state
        let current_vote_state = <VoteStates<T>>::get(first_prefix_key, vote_id)
            .ok_or(Error::<T>::VoteStateUninitialized)?;
        // check that the vote has not expired (could be commented out to not enforce if decided to not enforce)
        ensure!(
            !Self::check_vote_expired(current_vote_state.clone()),
            Error::<T>::VotePastExpirationTimeSoVotesNotAccepted
        );
        let second_prefix_key = (organization, share_id, vote_id);
        let mintable_signal = <MintedSignal<T>>::get(second_prefix_key, voter)
            .ok_or(Error::<T>::NotEnoughSignalToVote)?;
        let minted_signal = if let Some(mag) = magnitude {
            ensure!(mintable_signal >= mag, Error::<T>::NotEnoughSignalToVote);
            mag
        } else {
            mintable_signal
        };
        // form the vote
        let vote = Self::Vote {
            direction,
            magnitude: minted_signal,
        };
        // get the new state by applying the vote
        let new_state = Self::apply_vote(current_vote_state, vote)?;
        // place new vote state in storage
        <VoteStates<T>>::insert(first_prefix_key, vote_id, new_state.clone());
        // get the new outcome using the vote_status
        let new_outcome = Self::check_vote_outcome(new_state)?;
        // insert new outcome
        <VoteOutcome<T>>::insert(first_prefix_key, vote_id, new_outcome);
        Ok(())
    }
}