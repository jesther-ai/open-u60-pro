//! One gate task for every page and action that touches the device: the
//! work is boxed, so the blocking-task glue is compiled once instead of
//! once per call site (about 3 KB each).

use std::sync::Arc;
use std::time::Duration;

use axum::response::Response;

use super::render;
use super::store::{settle, Claim, Settled, Token};
use crate::auth::SessionId;
use crate::handlers::AppState;
use crate::httpd::gate::{GateError, Lane};
use crate::httpd::Ctx;

/// Blocking work that builds the whole response.
pub type Job = Box<dyn FnOnce(&AppState) -> Response + Send>;

/// Run `job` once a permit of `lane` is free. No permit in time: the busy
/// page with a "Try again" link to `retry`.
pub async fn run(ctx: &Ctx, lane: Lane, nonce: &str, retry: &str, job: Job) -> Response {
    try_run(ctx, lane, nonce, job).await.unwrap_or_else(|| render::busy(nonce, retry))
}

/// The same, `None` if no permit was free in time (`job` never ran and is
/// dropped).
pub async fn try_run(ctx: &Ctx, lane: Lane, nonce: &str, job: Job) -> Option<Response> {
    let app = Arc::clone(&ctx.app);
    match ctx.gate.run(lane, move || job(&app)).await {
        Ok(resp) => Some(resp),
        Err(GateError::Busy) => None,
        Err(GateError::Panicked) => Some(render::internal(nonce)),
    }
}

/// Work that sends something once: it gets the claimed token, disarms it
/// right before the device call and records the outcome (`Claim::set`).
pub type OnceJob = Box<dyn FnOnce(&AppState, &mut Claim) -> Response + Send>;

/// What a post with a single-use token came to.
pub enum Sent {
    /// The work ran (its answer).
    Ran(Response),
    /// No such fresh token: unknown, expired, another session's, or
    /// claimed for another post (an old copy of the form). Nothing sent.
    Expired,
    /// The same post again, after the first was sent: what it did.
    Done(Result<(), (u16, String)>),
    /// Still sending after `limit`, or the token is gone.
    Unknown,
    /// No permit in time: nothing was sent (the token is given back).
    Busy,
}

/// Run `work` once for `token` of `owner` (the post's `digest`): checked
/// first, claimed for this post, given back if the work never reaches the
/// device. A second post of a claimed token waits (at most `limit`) for
/// the first and reports what it did.
#[allow(clippy::too_many_arguments)]
pub async fn once(ctx: &Ctx, owner: SessionId, nonce: &str, token: &str, digest: u64, lane: Lane, limit: Duration, work: OnceJob) -> Sent {
    let (once, auth) = (Arc::clone(&ctx.once), Arc::clone(&ctx.app.auth));
    let claim = match once.get(token, owner, auth.now()) {
        None => return Sent::Expired,
        Some(Token::Fresh) => Claim::take(&once, &auth, token, owner, digest),
        Some(_) => None,
    };
    let mut claim = match claim {
        Some(c) => c,
        None => match settle(&once, token, owner, digest, &auth, limit).await {
            Settled::Other => return Sent::Expired,
            Settled::Sent => return Sent::Done(Ok(())),
            Settled::Failed(status, msg) => return Sent::Done(Err((status, msg))),
            Settled::Fresh => match Claim::take(&once, &auth, token, owner, digest) {
                Some(c) => c,
                None => return Sent::Unknown,
            },
            Settled::Unknown => return Sent::Unknown,
        },
    };
    match try_run(ctx, lane, nonce, Box::new(move |app: &AppState| work(app, &mut claim))).await {
        Some(resp) => Sent::Ran(resp),
        None => Sent::Busy,
    }
}

/// A digest of a form's fields (never stored: only compared).
pub fn digest(parts: &[&str]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}
