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

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use parking_lot::RwLock;
use prost::Message;
use risingwave_pb::catalog::PbSecret;
use risingwave_pb::secret::secret_ref::RefAsType;
use risingwave_pb::secret::PbSecretRef;
use thiserror_ext::AsReport;

use super::error::{SecretError, SecretResult};
use super::SecretId;

static INSTANCE: std::sync::OnceLock<LocalSecretManager> = std::sync::OnceLock::new();

#[derive(Debug)]
pub struct LocalSecretManager {
    secrets: RwLock<HashMap<SecretId, Vec<u8>>>,
    /// The local directory used to write secrets into file, so that it can be passed into some libararies
    secret_file_dir: PathBuf,
}

impl LocalSecretManager {
    /// Initialize the secret manager with the given temp file path, cluster id, and encryption key.
    /// # Panics
    /// Panics if fail to create the secret file directory.
    pub fn init(temp_file_dir: String, cluster_id: String, worker_id: u32) {
        // use `get_or_init` to handle concurrent initialization in single node mode.
        INSTANCE.get_or_init(|| {
            let secret_file_dir = PathBuf::from(temp_file_dir)
                .join(cluster_id)
                .join(worker_id.to_string());
            std::fs::remove_dir_all(&secret_file_dir).ok();

            // This will cause file creation conflict in simulation tests.
            // Should skip testing secret files in simulation tests.
            #[cfg(not(madsim))]
            std::fs::create_dir_all(&secret_file_dir).unwrap();

            Self {
                secrets: RwLock::new(HashMap::new()),
                secret_file_dir,
            }
        });
    }

    /// Get the global secret manager instance.
    /// # Panics
    /// Panics if the secret manager is not initialized.
    pub fn global() -> &'static LocalSecretManager {
        // Initialize the secret manager for unit tests.
        #[cfg(debug_assertions)]
        LocalSecretManager::init("./tmp".to_string(), "test_cluster".to_string(), 0);

        INSTANCE.get().unwrap()
    }

    pub fn add_secret(&self, secret_id: SecretId, secret: Vec<u8>) {
        let mut secret_guard = self.secrets.write();
        secret_guard.insert(secret_id, secret);
    }

    pub fn init_secrets(&self, secrets: Vec<PbSecret>) {
        let mut secret_guard = self.secrets.write();
        // Reset the secrets
        secret_guard.clear();
        // Error should only occurs when running simulation tests when we have multiple nodes
        // in 1 process and can fail .
        std::fs::remove_dir_all(&self.secret_file_dir)
            .inspect_err(|e| {
                tracing::error!(
            error = %e.as_report(),
            path = %self.secret_file_dir.to_string_lossy(),
            "Failed to remove secret directory")
            })
            .ok();

        #[cfg(not(madsim))]
        std::fs::create_dir_all(&self.secret_file_dir).unwrap();

        for secret in secrets {
            secret_guard.insert(secret.id, secret.value);
        }
    }

    pub fn get_secret(&self, secret_id: SecretId) -> Option<Vec<u8>> {
        let secret_guard = self.secrets.read();
        secret_guard.get(&secret_id).cloned()
    }

    pub fn remove_secret(&self, secret_id: SecretId) {
        let mut secret_guard = self.secrets.write();
        secret_guard.remove(&secret_id);
        self.remove_secret_file_if_exist(&secret_id);
    }

    pub fn fill_secrets(
        &self,
        mut options: BTreeMap<String, String>,
        secret_refs: BTreeMap<String, PbSecretRef>,
    ) -> SecretResult<BTreeMap<String, String>> {
        let secret_guard = self.secrets.read();
        for (option_key, secret_ref) in secret_refs {
            let secret_id = secret_ref.secret_id;
            let pb_secret_bytes = secret_guard
                .get(&secret_id)
                .ok_or(SecretError::ItemNotFound(secret_id))?;
            let secret_value_bytes = Self::get_secret_value(pb_secret_bytes)?;
            match secret_ref.ref_as() {
                RefAsType::Text => {
                    // We converted the secret string from sql to bytes using `as_bytes` in frontend.
                    // So use `from_utf8` here to convert it back to string.
                    options.insert(option_key, String::from_utf8(secret_value_bytes.clone())?);
                }
                RefAsType::File => {
                    let path_str =
                        self.get_or_init_secret_file(secret_id, secret_value_bytes.clone())?;
                    options.insert(option_key, path_str);
                }
                RefAsType::Unspecified => {
                    return Err(SecretError::UnspecifiedRefType(secret_id));
                }
            }
        }
        Ok(options)
    }

    /// Get the secret file for the given secret id and return the path string. If the file does not exist, create it.
    /// WARNING: This method should be called only when the secret manager is locked.
    fn get_or_init_secret_file(
        &self,
        secret_id: SecretId,
        secret_bytes: Vec<u8>,
    ) -> SecretResult<String> {
        let path = self.secret_file_dir.join(secret_id.to_string());
        if !path.exists() {
            let mut file = File::create(&path)?;
            file.write_all(&secret_bytes)?;
            file.sync_all()?;
        }
        Ok(path.to_string_lossy().to_string())
    }

    /// WARNING: This method should be called only when the secret manager is locked.
    fn remove_secret_file_if_exist(&self, secret_id: &SecretId) {
        let path = self.secret_file_dir.join(secret_id.to_string());
        if path.exists() {
            std::fs::remove_file(&path)
                .inspect_err(|e| {
                    tracing::error!(
                error = %e.as_report(),
                path = %path.to_string_lossy(),
                "Failed to remove secret file")
                })
                .ok();
        }
    }

    fn get_secret_value(pb_secret_bytes: &[u8]) -> SecretResult<Vec<u8>> {
        let pb_secret = risingwave_pb::secret::Secret::decode(pb_secret_bytes)
            .context("failed to decode secret")?;
        let secret_value = match pb_secret.get_secret_backend().unwrap() {
            risingwave_pb::secret::secret::SecretBackend::Meta(backend) => backend.value.clone(),
            risingwave_pb::secret::secret::SecretBackend::HashicorpVault(_) => {
                return Err(anyhow!("hashicorp_vault backend is not implemented yet").into())
            }
        };
        Ok(secret_value)
    }
}
