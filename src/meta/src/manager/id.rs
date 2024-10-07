// Copyright 2024 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use risingwave_common::catalog::NON_RESERVED_USER_ID;
use risingwave_hummock_sdk::compaction_group::StaticCompactionGroupId;
use thiserror_ext::AsReport;
use tokio::sync::RwLock;

use crate::manager::cluster::META_NODE_ID;
use crate::model::MetadataModelResult;
use crate::storage::{MetaStore, MetaStoreError, MetaStoreRef, DEFAULT_COLUMN_FAMILY};

pub const ID_PREALLOCATE_INTERVAL: u64 = 1000;

pub type Id = u64;

// TODO: remove unnecessary async trait.
#[async_trait::async_trait]
pub trait IdGenerator: Sync + Send + 'static {
    /// Generate a batch of identities.
    /// The valid id range will be [result_id, result_id + interval)
    async fn generate_interval(&self, interval: u64) -> MetadataModelResult<Id>;

    /// Generate an identity.
    async fn generate(&self) -> MetadataModelResult<Id> {
        self.generate_interval(1).await
    }
}

/// [`StoredIdGenerator`] implements id generator using metastore.
pub struct StoredIdGenerator {
    meta_store: MetaStoreRef,
    category_gen_key: String,
    current_id: AtomicU64,
    next_allocate_id: RwLock<Id>,
}

impl StoredIdGenerator {
    pub async fn new(meta_store: MetaStoreRef, category: &str, start: Option<Id>) -> Self {
        let category_gen_key = format!("{}_id_next_generator", category);
        let res = meta_store
            .get_cf(DEFAULT_COLUMN_FAMILY, category_gen_key.as_bytes())
            .await;
        let current_id = match res {
            Ok(value) => memcomparable::from_slice(&value).unwrap(),
            Err(MetaStoreError::ItemNotFound(_)) => start.unwrap_or(0),
            Err(e) => panic!("{}", e.as_report()),
        };

        let next_allocate_id = current_id + ID_PREALLOCATE_INTERVAL;
        if let Err(err) = meta_store
            .put_cf(
                DEFAULT_COLUMN_FAMILY,
                category_gen_key.clone().into_bytes(),
                memcomparable::to_vec(&next_allocate_id).unwrap(),
            )
            .await
        {
            panic!("{}", err.as_report());
        }

        StoredIdGenerator {
            meta_store,
            category_gen_key,
            current_id: AtomicU64::new(current_id),
            next_allocate_id: RwLock::new(next_allocate_id),
        }
    }
}

#[async_trait::async_trait]
impl IdGenerator for StoredIdGenerator {
    async fn generate_interval(&self, interval: u64) -> MetadataModelResult<Id> {
        let id = self.current_id.fetch_add(interval, Ordering::Relaxed);
        let next_allocate_id = { *self.next_allocate_id.read().await };
        let request_id = id.checked_add(interval).unwrap();
        if request_id > next_allocate_id {
            let mut next = self.next_allocate_id.write().await;
            if request_id > *next {
                let weight =
                    num_integer::Integer::div_ceil(&(request_id - *next), &ID_PREALLOCATE_INTERVAL);
                let next_allocate_id = (*next)
                    .checked_add(ID_PREALLOCATE_INTERVAL * weight)
                    .unwrap();
                self.meta_store
                    .put_cf(
                        DEFAULT_COLUMN_FAMILY,
                        self.category_gen_key.clone().into_bytes(),
                        memcomparable::to_vec(&next_allocate_id).unwrap(),
                    )
                    .await?;
                *next = next_allocate_id;
            }
        }

        Ok(id)
    }
}

pub type IdCategoryType = u8;

// TODO: Use enum to replace this once [feature(adt_const_params)](https://github.com/rust-lang/rust/issues/95174) get completed.
#[expect(non_snake_case, non_upper_case_globals)]
pub mod IdCategory {
    use super::IdCategoryType;

    #[cfg(test)]
    pub const Test: IdCategoryType = 0;
    pub const Database: IdCategoryType = 1;
    pub const Schema: IdCategoryType = 2;
    pub const Table: IdCategoryType = 3;
    pub const Worker: IdCategoryType = 4;
    pub const Fragment: IdCategoryType = 5;
    pub const Actor: IdCategoryType = 6;
    pub const Backup: IdCategoryType = 7;
    pub const HummockSstableId: IdCategoryType = 8;
    pub const _Source: IdCategoryType = 10;
    pub const HummockCompactionTask: IdCategoryType = 11;
    pub const User: IdCategoryType = 12;
    pub const _Sink: IdCategoryType = 13;
    pub const _Index: IdCategoryType = 14;
    pub const CompactionGroup: IdCategoryType = 15;
    pub const Function: IdCategoryType = 16;
    pub const Connection: IdCategoryType = 17;

    pub const Secret: IdCategoryType = 18;
}

pub type IdGeneratorManagerRef = Arc<IdGeneratorManager>;

/// [`IdGeneratorManager`] manages id generators in all categories,
/// which defined as [`IdCategory`] in [`meta.proto`].
pub struct IdGeneratorManager {
    #[cfg(test)]
    test: Arc<StoredIdGenerator>,
    database: Arc<StoredIdGenerator>,
    schema: Arc<StoredIdGenerator>,
    table: Arc<StoredIdGenerator>,
    function: Arc<StoredIdGenerator>,
    worker: Arc<StoredIdGenerator>,
    fragment: Arc<StoredIdGenerator>,
    actor: Arc<StoredIdGenerator>,
    user: Arc<StoredIdGenerator>,
    backup: Arc<StoredIdGenerator>,
    hummock_ss_table_id: Arc<StoredIdGenerator>,
    hummock_compaction_task: Arc<StoredIdGenerator>,
    compaction_group: Arc<StoredIdGenerator>,
    connection: Arc<StoredIdGenerator>,
    secret: Arc<StoredIdGenerator>,
}

impl IdGeneratorManager {
    pub async fn new(meta_store: MetaStoreRef) -> Self {
        Self {
            #[cfg(test)]
            test: Arc::new(StoredIdGenerator::new(meta_store.clone(), "test", None).await),
            database: Arc::new(StoredIdGenerator::new(meta_store.clone(), "database", None).await),
            schema: Arc::new(StoredIdGenerator::new(meta_store.clone(), "schema", None).await),
            table: Arc::new(StoredIdGenerator::new(meta_store.clone(), "table", Some(1)).await),
            function: Arc::new(StoredIdGenerator::new(meta_store.clone(), "function", None).await),
            worker: Arc::new(
                StoredIdGenerator::new(meta_store.clone(), "worker", Some(META_NODE_ID as u64 + 1))
                    .await,
            ),
            fragment: Arc::new(
                StoredIdGenerator::new(meta_store.clone(), "fragment", Some(1)).await,
            ),
            actor: Arc::new(StoredIdGenerator::new(meta_store.clone(), "actor", Some(1)).await),
            user: Arc::new(
                StoredIdGenerator::new(
                    meta_store.clone(),
                    "user",
                    Some(NON_RESERVED_USER_ID as u64),
                )
                .await,
            ),
            backup: Arc::new(StoredIdGenerator::new(meta_store.clone(), "backup", Some(1)).await),
            hummock_ss_table_id: Arc::new(
                StoredIdGenerator::new(meta_store.clone(), "hummock_ss_table_id", Some(1)).await,
            ),
            hummock_compaction_task: Arc::new(
                StoredIdGenerator::new(meta_store.clone(), "hummock_compaction_task", Some(1))
                    .await,
            ),
            compaction_group: Arc::new(
                StoredIdGenerator::new(
                    meta_store.clone(),
                    "compaction_group",
                    Some(StaticCompactionGroupId::End as u64 + 1),
                )
                .await,
            ),
            connection: Arc::new(
                StoredIdGenerator::new(meta_store.clone(), "connection", None).await,
            ),
            secret: Arc::new(StoredIdGenerator::new(meta_store.clone(), "secret", None).await),
        }
    }

    const fn get<const C: IdCategoryType>(&self) -> &Arc<StoredIdGenerator> {
        match C {
            #[cfg(test)]
            IdCategory::Test => &self.test,
            IdCategory::Database => &self.database,
            IdCategory::Schema => &self.schema,
            IdCategory::Table => &self.table,
            IdCategory::Function => &self.function,
            IdCategory::Fragment => &self.fragment,
            IdCategory::Actor => &self.actor,
            IdCategory::User => &self.user,
            IdCategory::Backup => &self.backup,
            IdCategory::Worker => &self.worker,
            IdCategory::HummockSstableId => &self.hummock_ss_table_id,
            IdCategory::HummockCompactionTask => &self.hummock_compaction_task,
            IdCategory::CompactionGroup => &self.compaction_group,
            IdCategory::Connection => &self.connection,
            IdCategory::Secret => &self.secret,
            _ => unreachable!(),
        }
    }

    /// [`Self::generate`] function generates id as `current_id`.
    pub async fn generate<const C: IdCategoryType>(&self) -> MetadataModelResult<Id> {
        self.get::<C>().generate().await
    }

    /// [`Self::generate_interval`] function generates ids as [`current_id`, `current_id` +
    /// interval), the next id will be `current_id` + interval.
    pub async fn generate_interval<const C: IdCategoryType>(
        &self,
        interval: u64,
    ) -> MetadataModelResult<Id> {
        self.get::<C>().generate_interval(interval).await
    }
}

#[cfg(test)]
mod tests {
    use futures::future;

    use super::*;
    use crate::storage::{MemStore, MetaStoreBoxExt};

    #[tokio::test]
    async fn test_id_generator() -> MetadataModelResult<()> {
        let meta_store = MemStore::default().into_ref();
        let id_generator = StoredIdGenerator::new(meta_store.clone(), "default", None).await;
        let ids = future::join_all((0..10000).map(|_i| {
            let id_generator = &id_generator;
            async move { id_generator.generate().await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;
        assert_eq!(ids, (0..10000).collect::<Vec<_>>());

        let id_generator_two = StoredIdGenerator::new(meta_store.clone(), "default", None).await;
        let ids = future::join_all((0..10000).map(|_i| {
            let id_generator = &id_generator_two;
            async move { id_generator.generate().await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;
        assert_eq!(ids, (10000..20000).collect::<Vec<_>>());

        let id_generator_three = StoredIdGenerator::new(meta_store.clone(), "table", None).await;
        let ids = future::join_all((0..10000).map(|_i| {
            let id_generator = &id_generator_three;
            async move { id_generator.generate().await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;
        assert_eq!(ids, (0..10000).collect::<Vec<_>>());

        let actor_id_generator = StoredIdGenerator::new(meta_store.clone(), "actor", Some(1)).await;
        let ids = future::join_all((0..100).map(|_i| {
            let id_generator = &actor_id_generator;
            async move { id_generator.generate_interval(100).await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;

        let vec_expect = (0..100).map(|e| e * 100 + 1).collect::<Vec<_>>();
        assert_eq!(ids, vec_expect);

        let actor_id_generator_two = StoredIdGenerator::new(meta_store, "actor", None).await;
        let ids = future::join_all((0..100).map(|_i| {
            let id_generator = &actor_id_generator_two;
            async move { id_generator.generate_interval(10).await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;

        let vec_expect = (0..100).map(|e| 10001 + e * 10).collect::<Vec<_>>();
        assert_eq!(ids, vec_expect);

        Ok(())
    }

    #[tokio::test]
    async fn test_id_generator_manager() -> MetadataModelResult<()> {
        let meta_store = MemStore::default().into_ref();
        let manager = IdGeneratorManager::new(meta_store.clone()).await;
        let ids = future::join_all((0..10000).map(|_i| {
            let manager = &manager;
            async move { manager.generate::<{ IdCategory::Test }>().await }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;
        assert_eq!(ids, (0..10000).collect::<Vec<_>>());

        let ids = future::join_all((0..100).map(|_i| {
            let manager = &manager;
            async move {
                manager
                    .generate_interval::<{ IdCategory::Actor }>(9999)
                    .await
            }
        }))
        .await
        .into_iter()
        .collect::<MetadataModelResult<Vec<_>>>()?;
        let vec_expect = (0..100).map(|e| e * 9999 + 1).collect::<Vec<_>>();
        assert_eq!(ids, vec_expect);

        let manager = IdGeneratorManager::new(meta_store).await;
        let id = manager
            .generate_interval::<{ IdCategory::Actor }>(10)
            .await?;
        assert_eq!(id, 1000001);

        Ok(())
    }
}
