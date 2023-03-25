use std::{fmt::Debug, str::FromStr};

use async_trait::async_trait;
use ethers::types::{Chain, Eip1559TransactionRequest, TxHash};
use serde_json::to_string;
use sqlx::{query, query_as, types::Json, FromRow, MySqlPool};
use uuid::Uuid;

#[async_trait]
pub trait TransactionRepository: Sync + Send + Debug {
    async fn save(
        &self,
        id: Uuid,
        hash: TxHash,
        tx: Eip1559TransactionRequest,
        mined: bool,
        chain: Chain,
    ) -> anyhow::Result<()>;
    async fn get(&self, id: Uuid) -> anyhow::Result<Option<Request>>;
    async fn get_pending(&self, chain: Chain) -> anyhow::Result<Vec<Request>>;
    async fn update_many(&self, updates: Vec<RequestUpdate>) -> anyhow::Result<()>;
}

pub struct RequestUpdate {
    pub id: Uuid,
    pub mined: bool,
    pub hash: TxHash,
}

#[derive(FromRow, Clone, Debug)]
pub struct RequestRecord {
    pub id: String,
    pub tx: Json<Eip1559TransactionRequest>,
    pub hash: String,
    pub mined: bool,
    pub chain: u32, // TODO is this big enough? I think so
}

pub struct Request {
    pub id: Uuid,
    pub tx: Eip1559TransactionRequest,
    pub hash: TxHash,
    pub mined: bool,
    pub chain: Chain,
}

impl From<RequestRecord> for Request {
    fn from(record: RequestRecord) -> Self {
        Request {
            id: Uuid::parse_str(&record.id)
                .unwrap_or_else(|_| panic!("Failed to parse id from record {:?}", &record)),
            hash: TxHash::from_str(&record.hash)
                .unwrap_or_else(|_| panic!("Failed to parse TxHash from record {:?}", &record)),
            chain: Chain::try_from(record.chain)
                .unwrap_or_else(|_| panic!("Failed to parse chain from record {:?}", &record)),
            tx: record.tx.0,
            mined: record.mined,
        }
    }
}

impl From<Request> for RequestRecord {
    fn from(request: Request) -> Self {
        RequestRecord {
            id: request.id.to_string(),
            tx: Json(request.tx),
            hash: request.hash.to_string(),
            mined: request.mined,
            chain: request.chain as u32,
        }
    }
}

#[derive(Debug)]
pub struct DbTxRequestRepository {
    pool: MySqlPool,
}

impl Clone for DbTxRequestRepository {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl DbTxRequestRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TransactionRepository for DbTxRequestRepository {
    async fn save(
        &self,
        id: Uuid,
        hash: TxHash,
        tx: Eip1559TransactionRequest,
        mined: bool,
        chain: Chain,
    ) -> anyhow::Result<()> {
        query!(
            r#"
			INSERT INTO requests (id, hash, tx, mined, chain) 
			VALUES (?, ?, ?, ?, ?)
			"#,
            id.to_string(),
            format!("{:?}", hash),
            to_string(&tx)?,
            mined,
            chain as u32
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> anyhow::Result<Option<Request>> {
        let request = query_as!(
            RequestRecord,
            r#"
		SELECT id, hash, chain, mined as "mined: bool", tx as "tx: Json<Eip1559TransactionRequest>"
		FROM requests 
		WHERE id = ?
		"#,
            id.to_string()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(request.map(|r| r.into()))
    }

    async fn get_pending(&self, chain: Chain) -> anyhow::Result<Vec<Request>> {
        let records = query_as!(
            RequestRecord,
            r#"
			SELECT id, hash, chain, mined as "mined: bool", tx as "tx: Json<Eip1559TransactionRequest>"
			FROM requests 
			WHERE mined = ? and chain = ?
			"#,
            false,
            chain as u32
        )
        .fetch_all(&self.pool)
        .await?;

        let requests: Vec<Request> = records
            .iter()
            .map(|record| Request::from(record.clone()))
            .collect();

        Ok(requests)
    }

    async fn update_many(&self, updates: Vec<RequestUpdate>) -> anyhow::Result<()> {
        if !updates.is_empty() {
            let mut tx = self.pool.begin().await?;

            for RequestUpdate { id, mined, hash } in updates {
                query!(
                    r#"
						UPDATE requests
						SET hash = ?, mined = ?
						WHERE id = ?;
						"#,
                    format!("{:?}", hash),
                    mined,
                    id.to_string()
                )
                .execute(&mut tx)
                .await?;
            }

            tx.commit().await?;
        }

        Ok(())
    }
}
