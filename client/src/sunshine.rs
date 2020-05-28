use crate::error::Error;
#[cfg(feature = "light-client")]
use crate::light_client::ChainType;
use crate::runtime::{Client, Pair, PairSigner, Runtime};
use crate::srml::shares_atomic::*;
use ipfs_embed::{Config, Store};
use ipld_block_builder::BlockBuilder;
use std::path::Path;
use substrate_subxt::Signer;

pub struct Sunshine {
    signer: PairSigner,
    subxt: Client,
    ipld: BlockBuilder<Store>,
}

impl Sunshine {
    fn create_block_builder(db: &sled::Db) -> Result<BlockBuilder<Store>, Error> {
        let ipld_tree = db.open_tree("ipld_tree")?;
        let config = Config::from_tree(ipld_tree);
        let store = Store::new(config)?;
        Ok(BlockBuilder::new(store))
    }

    #[cfg(feature = "light-client")]
    pub async fn new<T: AsRef<Path>>(
        path: T,
        pair: Pair,
        chain: ChainType,
    ) -> Result<Self, Error> {
        let db = sled::open(path)?;
        let subxt_tree = db.open_tree("subxt_tree")?;
        let subxt = crate::light_client::build_light_client(subxt_tree, chain).await?;
        let signer = PairSigner::new(pair);
        let ipld = Self::create_block_builder(&db)?;
        Ok(Self {
            subxt,
            signer,
            ipld,
        })
    }

    #[cfg(not(feature = "light-client"))]
    pub async fn new<T: AsRef<Path>>(path: T, pair: Pair) -> Result<Self, Error> {
        let db = sled::open(path)?;
        let signer = PairSigner::new(pair);
        let subxt = crate::runtime::ClientBuilder::new().build().await?;
        let ipld = Self::create_block_builder(&db)?;
        Ok(Self {
            signer,
            subxt,
            ipld,
        })
    }

    pub async fn reserve_shares(
        &self,
        org: u32,
        share: u32,
    ) -> Result<SharesReservedEvent<Runtime>, Error> {
        self.subxt
            .reserve_shares_and_watch(&self.signer, org, share, self.signer.account_id())
            .await?
            .shares_reserved()
            .map_err(|e| substrate_subxt::Error::Codec(e))?
            .ok_or(Error::EventNotFound)
    }
}
