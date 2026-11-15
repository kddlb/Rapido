//! The bridge between GPUI and tokio: one runtime for all network/database work.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use jmap_http::{JmapClient, TokenAuth};
use jmap_proto::{Id, capability};
use rapido_store::{Store, SyncEngine};

use crate::config::Config;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rapido-io")
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
}

/// Run a future on the tokio runtime. The returned handle can be awaited from
/// any executor (including GPUI's); it yields an error only if the task panicked.
pub fn spawn<F>(fut: F) -> impl Future<Output = Result<F::Output>>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = runtime().spawn(fut);
    async move {
        handle
            .await
            .map_err(|e| anyhow!("background task failed: {e}"))
    }
}

/// Run a store/sync future on tokio and flatten its error.
pub async fn run<T, F>(fut: F) -> Result<T>
where
    F: Future<Output = rapido_store::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    Ok(spawn(fut).await??)
}

/// Start a long-running task on tokio that can be cancelled.
pub fn spawn_abortable<F>(fut: F) -> tokio::task::AbortHandle
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime().spawn(fut).abort_handle()
}

/// Run blocking work (keyring, files) off the UI thread.
pub async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    runtime()
        .spawn_blocking(f)
        .await
        .map_err(|e| anyhow!("background task failed: {e}"))?
}

/// The server rejected the credentials.
#[derive(Debug)]
pub struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The server rejected the token.")
    }
}

impl std::error::Error for Unauthorized {}

/// A signed-in account.
pub struct Account {
    pub config: Config,
    pub engine: Arc<SyncEngine>,
}

impl Account {
    pub fn username(&self) -> &str {
        &self.config.username
    }
}

/// Validate credentials by fetching the session, then open the local cache.
pub async fn sign_in(session_url: String, token: String) -> Result<Account> {
    let auth = Arc::new(TokenAuth::new(token.clone()));
    let client = JmapClient::connect(session_url.clone(), auth)
        .await
        .map_err(|e| match e {
            jmap_http::Error::Unauthorized => anyhow!(Unauthorized),
            e => anyhow!(e),
        })?;
    let session = client.session();
    let account_id: Id = session
        .primary_account(capability::MAIL)
        .cloned()
        .context("This account has no JMAP Mail access.")?;
    let config = Config {
        session_url,
        username: session.username.clone(),
        account_id: account_id.to_string(),
    };
    let store = open_store(&account_id)?;
    store.set_state(SESSION_KEY, &serde_json::to_string(&*session)?)?;
    let engine = Arc::new(SyncEngine::new(Arc::new(client), store, account_id));
    Ok(Account { config, engine })
}

/// Where the last good session is cached, so the app can start offline.
const SESSION_KEY: &str = "rapido:session";

fn open_store(account_id: &Id) -> Result<Arc<Store>> {
    let db = Config::database_path(account_id.as_str())?;
    Ok(Arc::new(
        Store::open(&db).with_context(|| format!("opening {}", db.display()))?,
    ))
}

/// Sign in with a saved config and the token from the keyring. When the server
/// can't be reached, start from the cached session so cached mail stays readable.
pub async fn resume(config: Config) -> Result<Account> {
    let token = config.token().context("no saved token")?;
    match sign_in(config.session_url.clone(), token.clone()).await {
        Ok(a) => Ok(a),
        Err(e) if config.account_id.is_empty() => Err(e),
        Err(e) => {
            if e.is::<Unauthorized>() {
                return Err(e);
            }
            tracing::info!("starting offline: {e}");
            let account_id = Id::from(config.account_id.as_str());
            let store = open_store(&account_id)?;
            let session = store
                .state(SESSION_KEY)?
                .context("no cached session for offline start")?;
            let client = JmapClient::from_session(
                config.session_url.clone(),
                Arc::new(TokenAuth::new(token)),
                serde_json::from_str(&session)?,
            )?;
            let engine = Arc::new(SyncEngine::new(Arc::new(client), store, account_id));
            Ok(Account { config, engine })
        }
    }
}
