//! The server-wide parts of the Sparkles admin API: the server, datasets, tasks, backups
//! and statistics.

use crate::client::{Client, read_bytes, read_json, read_typed};
use crate::dataset::Dataset;
use crate::error::Result;
use crate::routes;
use crate::types::{DatasetInfo, DatasetType, ServerInfo, Task, Whoami};
use serde_json::Value;
use std::time::Duration;

impl Client {
    /// `GET /$/ping`: whether the server answers.
    pub async fn ping(&self) -> Result<()> {
        let r = self.op_req(&routes::PING, &[])?;
        read_bytes(self.send(r).await?).await?;
        Ok(())
    }

    /// The server's description (`GET /$/server`).
    pub async fn server(&self) -> Result<ServerInfo> {
        let r = self.op_req(&routes::SERVER, &[])?;
        read_typed(self.send(r).await?).await
    }

    /// Who the client's credentials act for (`GET /$/whoami`).
    pub async fn whoami(&self) -> Result<Whoami> {
        let r = self.op_req(&routes::WHOAMI, &[])?;
        read_typed(self.send(r).await?).await
    }

    /// The datasets the caller may see (`GET /$/datasets`).
    pub async fn datasets(&self) -> Result<Vec<DatasetInfo>> {
        let r = self.op_req(&routes::LIST_DATASETS, &[])?;
        let l: crate::types::DatasetList = read_typed(self.send(r).await?).await?;
        Ok(l.datasets)
    }

    /// Create a dataset (`POST /$/datasets`) and return a handle on it.
    pub async fn create_dataset(&self, name: &str, kind: DatasetType) -> Result<Dataset> {
        let mut r = self.op_req(&routes::CREATE_DATASET, &[])?;
        r.param("dbName", name);
        r.param("dbType", kind.as_str());
        read_bytes(self.send(r).await?).await?;
        Ok(self.dataset(name))
    }

    /// Remove a dataset and its files (`DELETE /$/datasets/{ds}`).
    pub async fn delete_dataset(&self, name: &str) -> Result<()> {
        let r = self.op_req(&routes::DELETE_DATASET, &[("ds", name)])?;
        read_bytes(self.send(r).await?).await?;
        Ok(())
    }

    /// The background tasks (`GET /$/tasks`).
    pub async fn tasks(&self) -> Result<Vec<Task>> {
        let r = self.op_req(&routes::LIST_TASKS, &[])?;
        read_typed(self.send(r).await?).await
    }

    /// One task (`GET /$/tasks/{id}`).
    pub async fn task(&self, id: &str) -> Result<Task> {
        let r = self.op_req(&routes::GET_TASK, &[("id", id)])?;
        read_typed(self.send(r).await?).await
    }

    /// Cancel a task that accepts cancellation (`DELETE /$/tasks/{id}`).
    pub async fn cancel_task(&self, id: &str) -> Result<Task> {
        let r = self.op_req(&routes::CANCEL_TASK, &[("id", id)])?;
        read_typed(self.send(r).await?).await
    }

    /// Poll a task every `interval` until it ends, and return its last state. Combine it
    /// with a deadline through `tokio::time::timeout` when the wait must be bounded.
    pub async fn wait_for_task(&self, id: &str, interval: Duration) -> Result<Task> {
        loop {
            let t = self.task(id).await?;
            if t.is_finished() {
                return Ok(t);
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Fuseki's list of N-Quads backup files (`GET /$/backups-list`).
    pub async fn backup_files(&self) -> Result<Value> {
        let r = self.op_req(&routes::BACKUP_FILES, &[])?;
        read_json(self.send(r).await?).await
    }

    /// Fuseki's request statistics of every dataset (`GET /$/stats`).
    pub async fn stats(&self) -> Result<Value> {
        let r = self.op_req(&routes::SERVER_STATS, &[])?;
        read_json(self.send(r).await?).await
    }
}
