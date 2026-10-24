//! `GET /ui/op/{id}`: the progress of a change (mobile data, airplane
//! mode, network mode, a scan, a registration, an eSIM switch) until the
//! device shows it, or it times out. Read-only: a reload or a reconnecting
//! stream never repeats the change. Without JavaScript the page reloads
//! itself every 3 s while the change runs: about a minute for most; an eSIM
//! switch the agent keeps verifying for up to about 6.5 minutes (4 of
//! busy retries, then the card, modem and data checks).

use axum::extract::Request;
use axum::response::Response;
use maud::{html, Markup};

use crate::auth::Principal;
use crate::events::Topic;
use crate::httpd::gate::Lane;
use crate::httpd::Ctx;
use crate::web::kit;
use crate::web::ops::{is_op_id, OpStatus};
use crate::web::render::{self, Fragment, PageCtx, Views};
use crate::web::{job, live, CspNonce};

/// Re-rendered on any change of the operation's topics (`OpSpec::topics`).
pub const FRAGMENTS: [Fragment; 2] = [
    Fragment { id: "op-title", topics: &Topic::ALL, render: heading },
    Fragment { id: "op-progress", topics: &Topic::ALL, render: progress },
];

/// Where the operation of `v` stands.
pub fn status(v: &Views) -> Option<OpStatus> {
    Some(v.op.as_ref()?.status(v))
}

pub async fn page(ctx: Ctx, p: Principal, req: Request) -> Response {
    let nonce = CspNonce::of(&req);
    let id = req.uri().path().strip_prefix("/ui/op/").unwrap_or("").to_owned();
    let Some(op) = ctx.ops.get(&id, p.id, ctx.app.auth.now()).filter(|_| is_op_id(&id)) else {
        return render::not_found(&nonce);
    };
    let (n, retry) = (nonce.clone(), format!("/ui/op/{id}"));
    let names = op.spec.topics.contains(&Topic::EsimSwitch).then(|| std::sync::Arc::clone(&ctx.esim_names));
    let job = Box::new(move |app: &crate::handlers::AppState| {
        let mut views = super::read_topics(app, op.spec.topics);
        views.now = app.auth.now();
        views.op = Some(op);
        views.esim_names = names;
        let pending = status(&views).is_some_and(|s| !s.finished());
        let live = pending.then(|| live::url(&app.auth, &format!("/ui/live/op/{id}"))).flatten();
        let (back, parent) = op.spec.back;
        let pc = PageCtx {
            title: title(&views),
            tab: Some(op.spec.tab),
            nonce: &n,
            live: live.as_deref(),
            refresh: pending,
            lead: kit::back(back, parent),
            heading: Some(heading(&views)),
            ..PageCtx::default()
        };
        let body = html! {
            (render::live_snapshot(&format!("/ui/op/{id}")))
            (progress(&views))
        };
        render::html(200, render::page(&pc, body))
    });
    job::run(&ctx, Lane::General, &nonce, &retry, job).await
}

/// What the operation does, in the past tense once it has finished.
fn title(v: &Views) -> &'static str {
    match v.op.as_ref().and_then(|op| op.spec.view) {
        Some(view) => (view.title)(v),
        None => v.op.as_ref().zip(status(v)).map_or("", |(op, st)| op.title(st)),
    }
}

/// `#op-title`: the nav bar's heading, patched when the operation finishes.
pub fn heading(v: &Views) -> Markup {
    html! { h1 #op-title { (title(v)) } }
}

/// `#op-progress`: the steps, the time so far, the outcome and the way back.
pub fn progress(v: &Views) -> Markup {
    if let Some(view) = v.op.as_ref().and_then(|op| op.spec.view) {
        return (view.progress)(v);
    }
    let (Some(op), Some(st)) = (v.op.as_ref(), status(v)) else {
        return html! { section #op-progress .card {} };
    };
    let secs = v.now.saturating_duration_since(op.started).as_secs();
    let (back, parent) = op.spec.back;
    html! {
        section #op-progress .card {
            ol.steps {
                @for (i, step) in op.spec.steps.iter().enumerate() {
                    @let class = match st {
                        OpStatus::Pending { step: cur } if i == cur => "cur",
                        OpStatus::Pending { step: cur } if i > cur => "todo",
                        OpStatus::TimedOut { .. } | OpStatus::Failed { .. } => "todo",
                        _ => "done",
                    };
                    li class=(class) aria-current=[(class == "cur").then_some("step")] { (step) }
                }
            }
            @match st {
                OpStatus::Pending { .. } => p.cap { (secs) " s so far" },
                OpStatus::Done { msg } => p.msg.info role="status" { (msg) },
                OpStatus::TimedOut { msg } if op.spec.timeout_ok => p.msg.info role="status" { (msg) },
                OpStatus::TimedOut { msg } | OpStatus::Failed { msg } => p.msg.err role="alert" { (msg) },
            }
            p.actions { a.btn.btn-quiet[!st.finished()] href=(back) { "Back to " (parent) } }
        }
    }
}
