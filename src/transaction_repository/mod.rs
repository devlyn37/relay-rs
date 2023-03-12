use ethers::types::{Chain, Eip1559TransactionRequest, TxHash};
use serde_json::to_string;
use sqlx::{query, query_as, types::Json, FromRow, MySqlPool};
use uuid::Uuid;

#[derive(FromRow, Clone, Debug)]
pub struct Request {
    pub id: String,
    pub tx: Json<Eip1559TransactionRequest>,
    pub hash: String,
    pub mined: bool,
    pub chain: u32,
}

pub type RequestUpdate = (Uuid, bool, TxHash);

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

    pub async fn save(
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

    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<Request>> {
        let request = query_as!(
            Request,
            r#"
				SELECT id, hash, chain, mined as "mined: bool", tx as "tx: Json<Eip1559TransactionRequest>"
				FROM requests 
				WHERE id = ?
				"#,
            id.to_string()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(request)
    }

    pub async fn get_pending(&self, chain: Chain) -> anyhow::Result<Vec<Request>> {
        let requests = query_as!(
            Request,
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

        Ok(requests)
    }

    pub async fn update_many(&self, updates: Vec<RequestUpdate>) -> anyhow::Result<()> {
        if updates.len() > 0 {
            let mut tx = self.pool.begin().await?;

            for (id, mined, hash) in updates {
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
