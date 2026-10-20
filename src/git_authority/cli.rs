//! Deliberately small CLI boundary for an explicitly connected Git authority.
//!
//! Detection happens before legacy replay/writer guards.  A bound store either
//! uses this adapter or receives an explicit refusal; it can never fall back to
//! local publication.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use jiff::Timestamp;
use serde_json::{Value, json};

use super::{
    client::{Submission, SubmissionStatus},
    connection,
    coordination::Action,
    protocol::{Command as ProtocolCommand, Request},
    transport::GitTransport,
};
use crate::cli::{AuthorityCmd, Cli, Command, SessionCmd, open_store};
use crate::{MoteError, MoteResult, ids};

mod communications;
mod observations;

/// Returns `None` only for an ordinary command against a non-Git store.
pub fn run_if_remote(cli: &Cli) -> MoteResult<Option<i32>> {
    if matches!(
        &cli.command,
        Command::Authority {
            cmd: AuthorityCmd::Capabilities
        }
    ) {
        print_value(
            cli.json,
            serde_json::to_value(super::compatibility::binary_evidence()?)?,
        );
        return Ok(Some(0));
    }
    if matches!(
        &cli.command,
        Command::Authority {
            cmd: AuthorityCmd::LegacyProfile
        }
    ) {
        print_value(
            cli.json,
            json!({"schema":"mote.legacy-profile.v1","profile_id":super::legacy_profile::current()?}),
        );
        return Ok(Some(0));
    }
    let store = match open_store(cli.store.as_deref()) {
        Ok(store) => store,
        Err(MoteError::StoreNotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let pending = connection::setup_path(store.root()).exists()
        || store.local_dir().join("git-rollback.json").exists();
    let bound = connection::load_descriptor(store.root())?.is_some();
    let is_setup = matches!(
        &cli.command,
        Command::Authority {
            cmd: AuthorityCmd::InitGit { .. }
                | AuthorityCmd::MigrateGit { .. }
                | AuthorityCmd::RollbackGit { .. }
                | AuthorityCmd::Connect { .. }
                | AuthorityCmd::Resume { .. }
                | AuthorityCmd::Refresh
                | AuthorityCmd::Cached
                | AuthorityCmd::Retry { .. }
                | AuthorityCmd::Receipt { .. }
                | AuthorityCmd::ExportLegacy { .. }
        }
    );
    if !bound && !pending && !is_setup {
        return Ok(None);
    }
    let code = match &cli.command {
        Command::Authority { cmd } => authority_command(cli, &store, cmd)?,
        Command::Session {
            cmd:
                SessionCmd::Start {
                    as_actor,
                    ttl,
                    label,
                },
        } => {
            if ttl.is_some() {
                return Err(MoteError::Invalid(
                    "Git authority sessions use explicit release, so --ttl is unsupported".into(),
                ));
            }
            let actor = match as_actor {
                Some(actor) => store.resolve_actor(Some(actor))?,
                None => store.resolve_actor(cli.actor.as_deref())?,
            };
            let session = ids::new_session_id();
            let action = Action::SessionStart {
                label: label.clone().unwrap_or_default(),
            };
            let req = Request {
                version: 1,
                request_id: ulid::Ulid::new().to_string(),
                actor: actor.clone(),
                session: session.clone(),
                timestamp: ids::format_rfc3339(Timestamp::now()),
                command: ProtocolCommand::Coordination { action },
            };
            let (client, _scratch) = open_client(&store)?;
            let submission = client.submit(req, 4)?;
            render_session_start(
                cli.json,
                &submission,
                &actor,
                &session,
                &mut std::io::stdout().lock(),
                &mut std::io::stderr().lock(),
            )?
        }
        Command::Session {
            cmd: SessionCmd::End { id },
        } => {
            let session = id
                .clone()
                .or_else(|| std::env::var("MOTE_SESSION").ok())
                .ok_or_else(|| {
                    MoteError::Invalid("no session id (pass one or set MOTE_SESSION)".into())
                })?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let active = view
                .get("sessions")
                .and_then(|v| v.get(&session))
                .ok_or_else(|| MoteError::Invalid("no such remote session".into()))?;
            let token = active
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("invalid remote session".into()))?
                .to_owned();
            let req = request(
                &store,
                cli,
                Action::SessionEnd(super::coordination::lifecycle::SessionEnd {
                    expect_session: token,
                    reason: "ended by holder".into(),
                }),
                session,
            )?;
            finish_submission(cli.json, client.submit(req, 4)?)?
        }
        Command::New {
            title,
            priority,
            body,
            assignee,
            tags,
            deps,
            id,
        } => {
            if !tags.is_empty() || !deps.is_empty() || id.is_some() {
                return Err(MoteError::Invalid("Git authority `new` currently supports only atomic title/body/priority/assignee creation; --tag, --dep, and --id are refused".into()));
            }
            let session = required_session()?;
            let action = Action::TaskCreate {
                id: ids::new_bead_id(),
                title: title.clone(),
                body: crate::cli::resolve_optional_text(body.clone())?.unwrap_or_default(),
                priority: priority.unwrap_or(2),
                assignee: assignee.clone(),
            };
            submit_action(&store, cli, action, session)?
        }
        Command::Set { id, fields } => submit_patch(&store, cli, id, fields, required_session()?)?,
        Command::Assign { id, assignee } => submit_patch(
            &store,
            cli,
            id,
            &[format!("assignee={assignee}")],
            required_session()?,
        )?,
        Command::Tag { cmd } => {
            let session = required_session()?;
            let action = match cmd {
                crate::cli::TagCmd::Add { id, tags } if tags.len() == 1 => Action::TaskTag {
                    id: id.clone(),
                    tag: tags[0].clone(),
                    add: true,
                },
                crate::cli::TagCmd::Add { .. } => {
                    return Err(MoteError::Invalid(
                        "Git authority tag add accepts exactly one tag per transaction".into(),
                    ));
                }
                crate::cli::TagCmd::Rm { id, tag } => Action::TaskTag {
                    id: id.clone(),
                    tag: tag.clone(),
                    add: false,
                },
            };
            submit_action(&store, cli, action, session)?
        }
        Command::Dep { cmd } => {
            let session = required_session()?;
            let (id, parent, add, kind) = match cmd {
                crate::cli::DepCmd::Add {
                    child,
                    parent,
                    kind,
                } => (child, parent, true, kind),
                crate::cli::DepCmd::Rm {
                    child,
                    parent,
                    kind,
                } => (child, parent, false, kind),
            };
            if kind != "blocks" {
                return Err(MoteError::Invalid(
                    "Git authority supports only dependency kind `blocks`".into(),
                ));
            }
            submit_action(
                &store,
                cli,
                Action::TaskDependency {
                    id: id.clone(),
                    parent: parent.clone(),
                    add,
                },
                session,
            )?
        }
        Command::Rel { cmd } => {
            let session = required_session()?;
            let (id, parent, add, relation) = match cmd {
                crate::cli::RelCmd::Add {
                    child,
                    parent,
                    kind,
                } => (child, parent, true, kind),
                crate::cli::RelCmd::Rm {
                    child,
                    parent,
                    kind,
                } => (child, parent, false, kind),
            };
            submit_action(
                &store,
                cli,
                Action::TaskRelation {
                    id: id.clone(),
                    parent: parent.clone(),
                    relation: relation.clone(),
                    add,
                },
                session,
            )?
        }
        Command::Note {
            id,
            note_kind,
            text,
            stdin,
        } => {
            let session = required_session()?;
            let text = crate::cli::resolve_positional_text(text.clone(), *stdin, "note text")?;
            submit_action(
                &store,
                cli,
                Action::TaskNote {
                    id: id.clone(),
                    note_kind: note_kind.clone(),
                    text,
                },
                session,
            )?
        }
        Command::Claim { id, ttl } => {
            if ttl.is_some() {
                return Err(MoteError::Invalid(
                    "Git authority claims use explicit release, so --ttl is unsupported".into(),
                ));
            }
            submit_action(
                &store,
                cli,
                Action::ClaimAcquire { id: id.clone() },
                required_session()?,
            )?
        }
        Command::Release { id, force } => {
            if *force {
                return Err(MoteError::Invalid(
                    "--force is unsupported by Git authority release".into(),
                ));
            }
            let session = required_session()?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let claim = view
                .get("claims")
                .and_then(|v| v.get(id))
                .ok_or_else(|| MoteError::Invalid("no remote claim for task".into()))?;
            if claim.get("actor").and_then(serde_json::Value::as_str)
                != Some(store.resolve_actor(cli.actor.as_deref())?.as_str())
                || claim.get("session").and_then(serde_json::Value::as_str)
                    != Some(session.as_str())
            {
                return Err(MoteError::Invalid(
                    "only the exact claim holder may release a Git authority claim".into(),
                ));
            }
            let token = claim
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("invalid remote claim".into()))?
                .to_owned();
            let req = request(
                &store,
                cli,
                Action::ClaimRelease {
                    id: id.clone(),
                    token,
                },
                session,
            )?;
            finish_submission(cli.json, client.submit(req, 4)?)?
        }
        Command::Reserve {
            paths,
            issue,
            candidate,
            ttl,
        } => {
            if ttl.is_some() || candidate.is_some() {
                return Err(MoteError::Invalid(
                    "remote reserve does not support --ttl or candidate reservations".into(),
                ));
            }
            let id = issue
                .clone()
                .ok_or_else(|| MoteError::Invalid("remote reserve requires --issue".into()))?;
            submit_action(
                &store,
                cli,
                Action::ReservationAcquire {
                    id,
                    reservation: ids::new_reservation_id(),
                    paths: normalize_paths(paths)?,
                },
                required_session()?,
            )?
        }
        Command::Unreserve { rv, paths } => {
            if !paths.is_empty() {
                return Err(MoteError::Invalid(
                    "remote unreserve accepts only a reservation ID".into(),
                ));
            }
            let session = required_session()?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let reservation = view
                .get("reservations")
                .and_then(|v| v.get(rv))
                .ok_or_else(|| MoteError::Invalid("no such remote reservation".into()))?;
            let token = reservation
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("invalid remote reservation".into()))?
                .to_owned();
            let req = request(
                &store,
                cli,
                Action::ReservationRelease {
                    reservation: rv.clone(),
                    token,
                },
                session,
            )?;
            finish_submission(cli.json, client.submit(req, 4)?)?
        }
        Command::Begin {
            id,
            paths,
            note,
            ttl,
            announce,
        } => {
            if ttl.is_some() {
                return Err(MoteError::Invalid(
                    "remote begin does not support --ttl".into(),
                ));
            }
            let session = required_session()?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            if announce.is_some()
                && !snapshot
                    .manifest
                    .capabilities
                    .iter()
                    .any(|c| c == "coordination-announcements-v1")
            {
                return Err(MoteError::Invalid(
                    "this immutable authority profile does not support atomic begin announcements"
                        .into(),
                ));
            }
            let task = snapshot
                .state
                .coordination
                .task_view()?
                .get(id)
                .cloned()
                .ok_or_else(|| MoteError::Invalid("no such remote task".into()))?;
            let expect_status = task
                .get("clock")
                .and_then(|v| v.get("status"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("remote task has no status clock".into()))?
                .to_owned();
            let mut paths = paths.clone();
            for path in &mut paths {
                *path = crate::paths::normalize(path).map_err(MoteError::Invalid)?;
            }
            paths.sort();
            paths.dedup();
            let reservation = (!paths.is_empty()).then(ids::new_reservation_id);
            let note = crate::cli::resolve_optional_text(note.clone())?;
            let begin = super::coordination::lifecycle::Begin {
                id: id.clone(),
                expect_status,
                reservation,
                paths,
                note,
            };
            let action = match announce {
                Some(topic) => {
                    Action::AnnouncedBegin(super::coordination::announcements::AnnouncedBegin {
                        begin,
                        announcement: super::coordination::announcements::Announcement {
                            id: ids::new_post_id(),
                            topic: crate::cli::normalize_discussion_topic(topic)?,
                        },
                    })
                }
                None => Action::Begin(begin),
            };
            let req = request(&store, cli, action, session)?;
            finish_submission(cli.json, client.submit(req, 4)?)?
        }
        Command::Done { id, note, force } => {
            if *force {
                return Err(MoteError::Invalid(
                    "--force is unsupported by remote done".into(),
                ));
            }
            let session = required_session()?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let task = view
                .get("tasks")
                .and_then(|v| v.get(id))
                .ok_or_else(|| MoteError::Invalid("no such remote task".into()))?;
            let expect_status = task
                .get("clock")
                .and_then(|v| v.get("status"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("remote task has no status clock".into()))?
                .to_owned();
            let claim = view.get("claims").and_then(|v| v.get(id)).ok_or_else(|| {
                MoteError::Invalid("remote done requires the caller's claim".into())
            })?;
            let actor = store.resolve_actor(cli.actor.as_deref())?;
            if claim.get("actor").and_then(serde_json::Value::as_str) != Some(actor.as_str())
                || claim.get("session").and_then(serde_json::Value::as_str)
                    != Some(session.as_str())
            {
                return Err(MoteError::Invalid(
                    "remote done requires the exact claim holder session".into(),
                ));
            }
            let expect_claim = claim
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| MoteError::Invalid("invalid remote claim".into()))?
                .to_owned();
            let mut reservations = BTreeMap::new();
            if let Some(all) = view
                .get("reservations")
                .and_then(serde_json::Value::as_object)
            {
                for (rid, reservation) in all {
                    if reservation.get("task").and_then(serde_json::Value::as_str) == Some(id)
                        && reservation
                            .get("session")
                            .and_then(serde_json::Value::as_str)
                            == Some(session.as_str())
                        && reservation
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            == Some("open")
                    {
                        reservations.insert(
                            rid.clone(),
                            reservation
                                .get("token")
                                .and_then(serde_json::Value::as_str)
                                .ok_or_else(|| {
                                    MoteError::Invalid("invalid remote reservation".into())
                                })?
                                .to_owned(),
                        );
                    }
                }
            }
            let note =
                crate::cli::resolve_optional_text(note.clone())?.unwrap_or_else(|| "done".into());
            let req = request(
                &store,
                cli,
                Action::Done(super::coordination::lifecycle::Done {
                    id: id.clone(),
                    expect_status,
                    expect_claim,
                    reservations,
                    note,
                }),
                session,
            )?;
            finish_submission(cli.json, client.submit(req, 4)?)?
        }
        Command::Send { .. }
        | Command::Msg { .. }
        | Command::Discuss { .. }
        | Command::Inbox { .. } => communications::run(cli, &store)?,
        Command::Show { id } => show_task(cli, &store, id)?,
        Command::Next {
            ttl,
            tags,
            exclude,
            resume,
        } => {
            if ttl.is_some() || *resume {
                return Err(MoteError::Invalid("remote next does not support --ttl or --resume; recover an abandoned session explicitly".into()));
            }
            let mut tags = tags.clone();
            tags.sort();
            tags.dedup();
            let mut exclude = exclude.clone();
            exclude.sort();
            exclude.dedup();
            submit_action(
                &store,
                cli,
                Action::Next(super::coordination::lifecycle::Next {
                    tags,
                    exclude,
                    reservation: None,
                    paths: vec![],
                    note: None,
                }),
                required_session()?,
            )?
        }
        Command::Handoff {
            id,
            to,
            force,
            note,
            release,
            expect_holder,
            expect_claim,
            idempotency_key,
        } => {
            if *force || idempotency_key.is_some() {
                return Err(MoteError::Invalid("remote handoff refuses --force and legacy --idempotency-key; retry retained requests with authority retry".into()));
            }
            let session = required_session()?;
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let recipient = resolve_recipient(&view, to)?;
            let claim = &view["claims"][id];
            if expect_holder
                .as_deref()
                .is_some_and(|v| claim["actor"].as_str() != Some(v))
                || expect_claim
                    .as_deref()
                    .is_some_and(|v| claim["token"].as_str() != Some(v))
            {
                return Err(MoteError::Rejected(
                    "remote handoff caller's claim observation is stale".into(),
                ));
            }
            let action = Action::Handoff(super::coordination::lifecycle::Handoff {
                id: id.clone(),
                expect_status: string_at(&view["tasks"][id]["clock"], "status")?,
                expect_claim: string_at(claim, "token")?,
                reservations: reservation_tokens(&view, &session, Some(id))?,
                to_session: recipient,
                disposition: if *release {
                    super::coordination::lifecycle::Disposition::Release
                } else {
                    super::coordination::lifecycle::Disposition::Transfer
                },
                note: crate::cli::resolve_optional_text(note.clone())?
                    .unwrap_or_else(|| "handoff".into()),
            });
            finish_submission(
                cli.json,
                client.submit(request(&store, cli, action, session)?, 4)?,
            )?
        }
        Command::Board => {
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            print_value(cli.json, fresh_view(&snapshot)?);
            0
        }
        Command::Ready { graph } => {
            if *graph {
                return Err(MoteError::Invalid(
                    "remote ready --graph is unsupported".into(),
                ));
            }
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let ids = snapshot.state.coordination.ready_task_ids()?;
            let tasks = snapshot.state.coordination.task_view()?;
            let data: Vec<_> = ids
                .iter()
                .map(|id| json!({"id":id,"task":tasks[id]}))
                .collect();
            print_value(cli.json, view_with_data(&snapshot, json!(data)));
            0
        }
        Command::Preflight {
            issue,
            candidate,
            paths,
        } => {
            if candidate.is_some() {
                return Err(MoteError::Invalid(
                    "remote candidate reservations are unsupported".into(),
                ));
            }
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            if issue
                .as_ref()
                .is_some_and(|id| view["tasks"].get(id).is_none())
            {
                return Err(MoteError::Invalid("no such remote issue".into()));
            }
            let overlaps = overlapping_reservations(&view, &normalize_paths(paths)?);
            let conflict = !overlaps.is_empty();
            print_value(
                cli.json,
                view_with_data(&snapshot, json!({"issue":issue,"conflicts":overlaps})),
            );
            if conflict { 2 } else { 0 }
        }
        Command::WhoHas { path } => {
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            print_value(
                cli.json,
                view_with_data(
                    &snapshot,
                    json!(overlapping_reservations(
                        &view,
                        &normalize_paths(std::slice::from_ref(path))?
                    )),
                ),
            );
            0
        }
        Command::Session {
            cmd: SessionCmd::List { all },
        } => {
            let (client, _scratch) = open_client(&store)?;
            let snapshot = client.refresh()?;
            let mut sessions = snapshot.state.coordination.view()?["sessions"].clone();
            if !all {
                if let Some(sessions) = sessions.as_object_mut() {
                    sessions.retain(|_, s| s["status"] == "active");
                }
            }
            print_value(cli.json, view_with_data(&snapshot, sessions));
            0
        }
        Command::History {
            id,
            include_rejected,
        } => remote_history(cli, &store, id, *include_rejected)?,
        Command::Ls {
            status,
            tags,
            assignee,
            all,
            ready,
            graph,
        } => {
            if *graph || *ready {
                return Err(MoteError::Invalid("remote ls graph/ready modes are not implemented; use `mote ready` when available".into()));
            }
            list_tasks(
                cli,
                &store,
                status.as_deref(),
                tags,
                assignee.as_deref(),
                *all,
            )?
        }
        Command::Doctor
        | Command::Fsck { .. }
        | Command::Audit { .. }
        | Command::Events { .. }
        | Command::Watch { .. }
        | Command::Actor {
            cmd: crate::cli::ActorCmd::List { .. } | crate::cli::ActorCmd::Status { .. },
        } => observations::run(cli, &store)?,
        // Local identity configuration is safe, but projections of actor
        // activity must not fall through to stale local replay.
        Command::Actor {
            cmd:
                crate::cli::ActorCmd::Set { .. }
                | crate::cli::ActorCmd::Show
                | crate::cli::ActorCmd::Clear,
        } => return Ok(None),
        _ if pending => {
            return Err(MoteError::Invalid(
                "Git setup is incomplete; run `mote authority resume` before other commands".into(),
            ));
        }
        _ => {
            return Err(MoteError::Invalid(
                "this Git-authority command is not implemented by the remote adapter; local replay and publication are refused"
                    .into(),
            ));
        }
    };
    Ok(Some(code))
}

fn authority_command(cli: &Cli, store: &crate::repo::Store, cmd: &AuthorityCmd) -> MoteResult<i32> {
    match cmd {
        AuthorityCmd::Capabilities | AuthorityCmd::LegacyProfile => {
            unreachable!("profile diagnostic is handled before store discovery")
        }
        AuthorityCmd::ExportLegacy {
            output,
            cleanup_start,
        } => {
            let archive = super::legacy::capture(store, *cleanup_start)?;
            super::legacy::write_archive(&archive, output)?;
            print_value(
                cli.json,
                json!({"schema":"mote.legacy-export-receipt.v1","digest":archive.digest,"output":output}),
            );
            Ok(0)
        }
        AuthorityCmd::InitGit { remote, branch } => {
            let (transport, _scratch) = transport(store, remote, branch)?;
            let descriptor =
                connection::bootstrap(store, transport, &connection::default_cache_root(store))?;
            print_value(
                cli.json,
                json!({"operation":"bootstrap","descriptor":descriptor}),
            );
            Ok(0)
        }
        AuthorityCmd::MigrateGit {
            remote,
            branch,
            backup,
            cleanup_start,
        } => {
            super::legacy_profile::current()?;
            let (transport, _scratch) = transport(store, remote, branch)?;
            let descriptor = connection::migrate(
                store,
                transport,
                &connection::default_cache_root(store),
                &connection::MigrationOptions {
                    backup: backup.clone(),
                    cleanup_start: *cleanup_start,
                },
            )?;
            print_value(
                cli.json,
                json!({"operation":"migrate","descriptor":descriptor,"backup":backup}),
            );
            Ok(0)
        }
        AuthorityCmd::RollbackGit { output, reason } => {
            if let Some(completed) = super::rollback::completed(store, output, reason)? {
                print_value(cli.json, serde_json::to_value(completed)?);
                return Ok(0);
            }
            let (remote, branch) = super::rollback::route_hint(store)?;
            let (transport, _scratch) = transport(store, &remote, &branch)?;
            let result =
                super::rollback::run(store, transport, output, cli.actor.as_deref(), reason)?;
            print_value(cli.json, serde_json::to_value(result)?);
            Ok(0)
        }
        AuthorityCmd::Connect { remote, branch } => {
            let (transport, _scratch) = transport(store, remote, branch)?;
            let descriptor =
                connection::connect(store, transport, &connection::default_cache_root(store))?;
            print_value(
                cli.json,
                json!({"operation":"connect","descriptor":descriptor}),
            );
            Ok(0)
        }
        AuthorityCmd::Resume { remote, branch } => {
            let (transport, _scratch) = transport(store, remote, branch)?;
            let descriptor =
                connection::resume(store, transport, &connection::default_cache_root(store))?;
            print_value(
                cli.json,
                json!({"operation":"resume","descriptor":descriptor}),
            );
            Ok(0)
        }
        AuthorityCmd::Refresh => {
            let (client, _scratch) = open_client(store)?;
            let snapshot = client.refresh()?;
            print_value(cli.json, fresh_view(&snapshot)?);
            Ok(0)
        }
        AuthorityCmd::Cached => {
            let cached = connection::cached_view(store, &connection::default_cache_root(store))?;
            print_value(
                cli.json,
                json!({"schema":"mote.git-view.v1","authority":{"fresh":false,"head":cached.head,"root":cached.binding.root,"store_id":cached.binding.store_id,"generation":cached.binding.generation,"sequence":cached.sequence,"observed_at":cached.observed_at,"ownership":"explicit-release-v1"},"data":cached.values}),
            );
            Ok(0)
        }
        AuthorityCmd::Retry { request_id } => {
            let (client, _scratch) = open_client(store)?;
            finish_submission(cli.json, client.retry_pending(request_id, 4)?)
        }
        AuthorityCmd::Receipt { request_id } => {
            if !super::protocol::simple_identifier(request_id, 100) {
                return Err(MoteError::Invalid("invalid request ID".into()));
            }
            let (client, _scratch) = open_client(store)?;
            let snapshot = client.refresh()?;
            let receipt = snapshot.receipts.get(request_id);
            print_value(
                cli.json,
                view_with_data(
                    &snapshot,
                    json!({"request_id":request_id,"receipt":receipt,"absence_is_final":false}),
                ),
            );
            Ok(if receipt.is_some() { 0 } else { 2 })
        }
        // These commands are caught before old local code can invoke a writer.
        AuthorityCmd::Status => {
            let (client, _scratch) = open_client(store)?;
            let snapshot = client.refresh()?;
            print_value(
                cli.json,
                view_with_data(
                    &snapshot,
                    json!({"manifest":snapshot.manifest,"binary":super::compatibility::binary_evidence()?,"compatibility":super::compatibility::compare_manifest(&snapshot.manifest),"verified_legacy_replay":snapshot.manifest.legacy_digest.is_some()}),
                ),
            );
            Ok(0)
        }
        AuthorityCmd::RecoverSession {
            source_session,
            expect_session,
            reason,
            to_session,
        } => {
            let (client, _scratch) = open_client(store)?;
            let snapshot = client.refresh()?;
            let view = snapshot.state.coordination.view()?;
            let claims = view["claims"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, c)| c["session"].as_str() == Some(source_session))
                .map(|(id, c)| Ok((id.clone(), string_at(c, "token")?)))
                .collect::<MoteResult<BTreeMap<_, _>>>()?;
            let action = Action::SessionRecover(super::coordination::lifecycle::SessionRecover {
                source_session: source_session.clone(),
                expect_session: expect_session.clone(),
                claims,
                reservations: reservation_tokens(&view, source_session, None)?,
                disposition: if to_session.is_some() {
                    super::coordination::lifecycle::Disposition::Transfer
                } else {
                    super::coordination::lifecycle::Disposition::Release
                },
                recipient_session: to_session.clone(),
                reason: reason.clone(),
            });
            finish_submission(
                cli.json,
                client.submit(request(store, cli, action, required_session()?)?, 4)?,
            )
        }
        AuthorityCmd::Enable | AuthorityCmd::Migrate { .. } => Err(MoteError::Invalid(
            "legacy authority controls are unavailable for a Git-authority store".into(),
        )),
    }
}

fn fresh_view(snapshot: &super::history::Snapshot) -> MoteResult<serde_json::Value> {
    Ok(view_with_data(
        snapshot,
        snapshot.state.coordination.view()?,
    ))
}
fn view_with_data(snapshot: &super::history::Snapshot, data: Value) -> Value {
    json!({
        "schema":"mote.git-view.v1",
        "authority":{"fresh":true,"head":snapshot.head,"root":snapshot.root,"store_id":snapshot.manifest.store_id,"generation":snapshot.manifest.generation,"sequence":snapshot.state.sequence,"observed_at":ids::format_rfc3339(Timestamp::now()),"ownership":"explicit-release-v1"},
        "data":data,
    })
}

fn finish_submission(json_mode: bool, submission: Submission) -> MoteResult<i32> {
    let status = submission.status;
    let mut value = serde_json::to_value(&submission)?;
    value["recovery"] = json!({"retry":format!("mote authority retry {}",submission.request_id),"receipt":format!("mote authority receipt {}",submission.request_id)});
    let no_work = submission
        .receipt
        .as_ref()
        .is_some_and(|r| r.outcome.result["code"] == "no_ready_task");
    print_value(json_mode, value);
    Ok(match status {
        SubmissionStatus::Accepted => 0,
        SubmissionStatus::Rejected if no_work => 5,
        SubmissionStatus::Rejected => 2,
        SubmissionStatus::Unconfirmed => 4,
    })
}

fn submit_action(
    store: &crate::repo::Store,
    cli: &Cli,
    action: Action,
    session: String,
) -> MoteResult<i32> {
    let req = request(store, cli, action, session)?;
    let (client, _scratch) = open_client(store)?;
    finish_submission(cli.json, client.submit(req, 4)?)
}

fn submit_patch(
    store: &crate::repo::Store,
    cli: &Cli,
    id: &str,
    fields: &[String],
    session: String,
) -> MoteResult<i32> {
    let (client, _scratch) = open_client(store)?;
    let snapshot = client.refresh()?;
    let tasks = snapshot.state.coordination.task_view()?;
    let task = tasks
        .get(id)
        .ok_or_else(|| MoteError::Invalid("no such remote task".into()))?;
    let clock = task
        .get("clock")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MoteError::Invalid("invalid remote task clock".into()))?;
    let mut set = BTreeMap::new();
    let mut expect = BTreeMap::new();
    for raw in fields {
        let (field, value) = raw
            .split_once('=')
            .ok_or_else(|| MoteError::Invalid("set fields must be field=value".into()))?;
        let parsed = match field {
            "title" | "body" | "assignee" => serde_json::Value::String(value.into()),
            "priority" => serde_json::Value::Number(
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (0..=3).contains(n))
                    .ok_or_else(|| MoteError::Invalid("priority must be 0..=3".into()))?
                    .into(),
            ),
            "status" => {
                return Err(MoteError::Invalid(
                    "remote status changes use begin/done lifecycle commands".into(),
                ));
            }
            _ => return Err(MoteError::Invalid("unsupported remote patch field".into())),
        };
        if set.insert(field.into(), parsed).is_some() {
            return Err(MoteError::Invalid("duplicate remote patch field".into()));
        }
        expect.insert(
            field.into(),
            clock
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        );
    }
    let req = request(
        store,
        cli,
        Action::TaskPatch {
            id: id.into(),
            set,
            expect,
        },
        session,
    )?;
    finish_submission(cli.json, client.submit(req, 4)?)
}

fn show_task(cli: &Cli, store: &crate::repo::Store, id: &str) -> MoteResult<i32> {
    let (client, _scratch) = open_client(store)?;
    let snapshot = client.refresh()?;
    let task = snapshot
        .state
        .coordination
        .task_view()?
        .get(id)
        .cloned()
        .ok_or_else(|| MoteError::Invalid("no such remote task".into()))?;
    print_value(cli.json, view_with_data(&snapshot, task));
    Ok(0)
}

fn list_tasks(
    cli: &Cli,
    store: &crate::repo::Store,
    status: Option<&str>,
    tags: &[String],
    assignee: Option<&str>,
    all: bool,
) -> MoteResult<i32> {
    let (client, _scratch) = open_client(store)?;
    let snapshot = client.refresh()?;
    let mut tasks = snapshot.state.coordination.task_view()?;
    let obj = tasks
        .as_object_mut()
        .ok_or_else(|| MoteError::Invalid("invalid remote task projection".into()))?;
    obj.retain(|_, task| {
        status.is_none_or(|s| task.get("status").and_then(serde_json::Value::as_str) == Some(s))
            && assignee
                .is_none_or(|a| task.get("assignee").and_then(serde_json::Value::as_str) == Some(a))
            && tags.iter().all(|tag| {
                task.get("tags")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|actual| actual.iter().any(|v| v.as_str() == Some(tag)))
            })
            && (all || task.get("status").and_then(serde_json::Value::as_str) != Some("closed"))
    });
    print_value(cli.json, view_with_data(&snapshot, tasks));
    Ok(0)
}

fn required_session() -> MoteResult<String> {
    std::env::var("MOTE_SESSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MoteError::Invalid("remote mutation requires MOTE_SESSION; run `mote session start` and eval its activation output".into()))
}

fn normalize_paths(paths: &[String]) -> MoteResult<Vec<String>> {
    let mut paths = paths
        .iter()
        .map(|p| crate::paths::normalize(p).map_err(MoteError::Invalid))
        .collect::<MoteResult<Vec<_>>>()?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}
fn string_at(value: &Value, key: &str) -> MoteResult<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| MoteError::Invalid(format!("remote observation is missing {key}")))
}
fn reservation_tokens(
    view: &Value,
    session: &str,
    task: Option<&str>,
) -> MoteResult<BTreeMap<String, String>> {
    view["reservations"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, r)| {
            r["session"].as_str() == Some(session)
                && r["status"] == "open"
                && task.is_none_or(|id| r["task"].as_str() == Some(id))
        })
        .map(|(id, r)| Ok((id.clone(), string_at(r, "token")?)))
        .collect()
}
fn resolve_recipient(view: &Value, target: &str) -> MoteResult<String> {
    if view["sessions"][target]["status"] == "active" {
        return Ok(target.into());
    }
    let matches: Vec<_> = view["sessions"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, s)| s["status"] == "active" && s["actor"].as_str() == Some(target))
        .map(|(id, _)| id.clone())
        .collect();
    match matches.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(MoteError::Invalid(
            "handoff recipient has no active remote session".into(),
        )),
        _ => Err(MoteError::Invalid(
            "handoff recipient has multiple active sessions; name the exact session ID".into(),
        )),
    }
}
fn overlapping_reservations(view: &Value, paths: &[String]) -> BTreeMap<String, Value> {
    view["reservations"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, r)| {
            r["status"] == "open"
                && r["paths"].as_array().is_some_and(|held| {
                    held.iter()
                        .filter_map(Value::as_str)
                        .any(|a| paths.iter().any(|b| crate::paths::overlap(a, b)))
                })
        })
        .map(|(id, r)| (id.clone(), r.clone()))
        .collect()
}
fn remote_history(
    cli: &Cli,
    store: &crate::Store,
    id: &str,
    include_rejected: bool,
) -> MoteResult<i32> {
    let (client, _scratch) = open_client(store)?;
    let snapshot = client.refresh()?;
    let view = snapshot.state.coordination.view()?;
    let mut entries = Vec::new();
    if let Some(pack) = snapshot.files.get("legacy.pack") {
        // The fresh snapshot already validated this exact root archive and its
        // pinned replay. Retain source positions instead of fabricating remote
        // request IDs or ordering old events by timestamp.
        let files = super::legacy::unpack(pack)?;
        let decisions: Value = super::protocol::decode(&files["legacy/decisions.json"])?;
        for input in decisions["inputs"]
            .as_array()
            .ok_or_else(|| MoteError::Invalid("invalid validated legacy history".into()))?
        {
            if let Some(events) = input["entity_events"][id].as_array() {
                for (event_index, event) in events.iter().enumerate() {
                    if include_rejected || event["accepted"] == Value::Bool(true) {
                        entries.push(json!({"phase":"legacy","ordinal":input["ordinal"],"event_index":event_index,"event":event}));
                    }
                }
            }
        }
    }
    for (path, bytes) in &snapshot.files {
        if !path.starts_with("transactions/") {
            continue;
        }
        let tx: super::protocol::Transaction = super::protocol::decode(bytes)?;
        let accepted = tx.outcome.status == super::protocol::OutcomeStatus::Accepted;
        if !include_rejected && !accepted {
            continue;
        }
        let command = serde_json::to_value(&tx.request.command)?;
        let action = &command["action"];
        let reservation_task = |rid: &str| view["reservations"][rid]["task"].as_str() == Some(id);
        let touches = action["id"].as_str() == Some(id)
            || action["claims"].get(id).is_some()
            || action["reservation"].as_str().is_some_and(reservation_task)
            || action["reservations"]
                .as_object()
                .is_some_and(|all| all.keys().any(|rid| reservation_task(rid)))
            || tx.effects.iter().any(|effect| {
                let value = serde_json::to_value(effect).expect("validated effect serializes");
                value["op"]["entity"].as_str() == Some(id)
                    || value["transition"]["task"].as_str() == Some(id)
            });
        if touches {
            entries.push(json!({"phase":"remote","sequence":tx.sequence,"request_id":tx.request.request_id,"actor":tx.request.actor,"session":tx.request.session,"timestamp":tx.request.timestamp,"outcome":tx.outcome,"effects":tx.effects}));
        }
    }
    print_value(cli.json, view_with_data(&snapshot, json!(entries)));
    Ok(0)
}

/// Fix the timestamp once, so an eventual retry
/// uses the journaled canonical request rather than reconstructing intent.
fn request(
    store: &crate::repo::Store,
    cli: &Cli,
    action: Action,
    session: String,
) -> MoteResult<Request> {
    Ok(Request {
        version: 1,
        request_id: ulid::Ulid::new().to_string(),
        actor: store.resolve_actor(cli.actor.as_deref())?,
        session,
        timestamp: ids::format_rfc3339(Timestamp::now()),
        command: ProtocolCommand::Coordination { action },
    })
}

fn open_client(store: &crate::repo::Store) -> MoteResult<(super::client::Client, Scratch)> {
    let descriptor = connection::load_descriptor(store.root())?
        .ok_or_else(|| MoteError::Invalid("store has no Git authority descriptor".into()))?;
    let cache = connection::default_cache_root(store);
    let remote = connection::selected_remote(store, &cache)?;
    let (transport, scratch) = transport(store, &remote, &descriptor.branch)?;
    Ok((connection::open_client(store, transport, &cache)?, scratch))
}

fn transport(
    store: &crate::Store,
    remote: &str,
    branch: &str,
) -> MoteResult<(GitTransport, Scratch)> {
    super::transport::require_project_git_environment()?;
    let cwd = std::env::current_dir()?;
    let store_repo = store
        .root()
        .parent()
        .ok_or_else(|| MoteError::Invalid("store has no project directory".into()))?;
    if git_common_dir(&cwd)? != git_common_dir(store_repo)? {
        return Err(MoteError::Invalid(
            "selected store belongs to a different Git repository; use a checkout of that project"
                .into(),
        ));
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !output.status.success() {
        return Err(MoteError::Invalid(
            "Git authority requires a project checkout".into(),
        ));
    }
    let repo = PathBuf::from(
        String::from_utf8(output.stdout)
            .map_err(|_| MoteError::Invalid("non-UTF8 project path".into()))?
            .trim(),
    );
    let scratch = Scratch::new()?;
    let transport = GitTransport::open(&repo, remote, branch, scratch.path())?;
    Ok((transport, scratch))
}

fn git_common_dir(directory: &Path) -> MoteResult<PathBuf> {
    let out = ProcessCommand::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--git-common-dir"])
        .output()?;
    if !out.status.success() {
        return Err(MoteError::Invalid(
            "Git authority requires the selected store inside its project's Git repository".into(),
        ));
    }
    let path = PathBuf::from(
        String::from_utf8(out.stdout)
            .map_err(|_| MoteError::Invalid("non-UTF8 Git path".into()))?
            .trim(),
    );
    Ok(if path.is_absolute() {
        path
    } else {
        directory.join(path)
    }
    .canonicalize()?)
}

fn render_session_start(
    json_mode: bool,
    submission: &Submission,
    actor: &str,
    session: &str,
    out: &mut impl Write,
    err: &mut impl Write,
) -> MoteResult<i32> {
    let accepted = submission.status == SubmissionStatus::Accepted;
    let activate = format!(
        "export MOTE_ACTOR={}; export MOTE_SESSION={}",
        crate::cli::shell_quote(actor),
        crate::cli::shell_quote(session)
    );
    if json_mode {
        let value = if accepted {
            json!({"submission":submission,"session_id":session,"actor":actor,"activate":activate})
        } else {
            serde_json::to_value(submission)?
        };
        writeln!(out, "{}", serde_json::to_string(&value)?)?;
    } else if accepted {
        writeln!(out, "{activate}")?;
    } else {
        writeln!(
            err,
            "{}\nRecover with: mote authority retry {}",
            serde_json::to_string(submission)?,
            submission.request_id
        )?;
    }
    Ok(match submission.status {
        SubmissionStatus::Accepted => 0,
        SubmissionStatus::Rejected => 2,
        SubmissionStatus::Unconfirmed => 4,
    })
}

/// A command-owned transport area.  It is never inside the source checkout,
/// and Drop only removes the exact directory this invocation created.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> MoteResult<Self> {
        let path = std::env::temp_dir().join(format!("mote-cli-git-{}", ulid::Ulid::new()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn print_value(json_mode: bool, value: serde_json::Value) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&value).expect("serializable CLI value")
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("serializable CLI value")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_and_unconfirmed_enrollment_emit_no_shell_activation() {
        for status in [SubmissionStatus::Rejected, SubmissionStatus::Unconfirmed] {
            let submission = Submission {
                status,
                request_id: "enrollment-request".into(),
                attempted_commit: Some("a".repeat(40)),
                receipt: None,
                recovered: false,
                diagnostic: None,
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code =
                render_session_start(false, &submission, "actor", "session", &mut out, &mut err)
                    .unwrap();
            assert!(out.is_empty());
            assert_ne!(code, 0);
            assert!(
                String::from_utf8(err)
                    .unwrap()
                    .contains("authority retry enrollment-request")
            );
        }
    }
}
