//! Allows undoing to a previous state of the repo.
//!
//! This is accomplished by finding the events that have happened since a certain
//! time and inverting them.

use std::convert::TryInto;
use std::io::{stdin, stdout, BufRead, BufReader, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::SystemTime;

use anyhow::Context;
use cursive::event::Key;
use cursive::utils::markup::StyledString;
use cursive::views::{Dialog, EditView, LinearLayout, OnEventView, ScrollView, TextView};
use cursive::{Cursive, CursiveRunnable, CursiveRunner};

use crate::commands::smartlog::render_graph;
use crate::core::eventlog::{Event, EventCursor, EventLogDb, EventReplayer, EventTransactionId};
use crate::core::formatting::{printable_styled_string, Glyphs, Pluralize, StyledStringBuilder};
use crate::core::graph::{make_graph, BranchOids, HeadOid, MainBranchOid};
use crate::core::mergebase::MergeBaseDb;
use crate::core::metadata::{
    render_commit_metadata, BranchesProvider, CommitMessageProvider, CommitOidProvider,
    DifferentialRevisionProvider, HiddenExplanationProvider, RelativeTimeProvider,
};
use crate::core::tui::{with_siv, SingletonView};
use crate::declare_views;
use crate::util::{get_db_conn, get_repo, run_git, GitExecutable};

fn render_cursor_smartlog(
    glyphs: &Glyphs,
    repo: &git2::Repository,
    merge_base_db: &MergeBaseDb,
    event_replayer: &EventReplayer,
    event_cursor: EventCursor,
) -> anyhow::Result<Vec<StyledString>> {
    let head_oid = event_replayer.get_cursor_head_oid(event_cursor);
    let main_branch_oid = event_replayer.get_cursor_main_branch_oid(event_cursor, repo)?;
    let branch_oid_to_names = event_replayer.get_cursor_branch_oid_to_names(event_cursor, repo)?;
    let graph = make_graph(
        repo,
        merge_base_db,
        event_replayer,
        event_cursor,
        &HeadOid(head_oid),
        &MainBranchOid(main_branch_oid),
        &BranchOids(branch_oid_to_names.keys().copied().collect()),
        true,
    )?;
    let result = render_graph(
        glyphs,
        repo,
        merge_base_db,
        &graph,
        &HeadOid(head_oid),
        &mut [
            &mut CommitOidProvider::new(true)?,
            &mut RelativeTimeProvider::new(&repo, SystemTime::now())?,
            &mut HiddenExplanationProvider::new(&graph, &event_replayer, event_cursor)?,
            &mut BranchesProvider::new(&repo, &branch_oid_to_names)?,
            &mut DifferentialRevisionProvider::new(&repo)?,
            &mut CommitMessageProvider::new()?,
        ],
    )?;
    Ok(result)
}

fn render_ref_name(ref_name: &str) -> String {
    match ref_name.strip_prefix("refs/heads/") {
        Some(branch_name) => format!("branch {}", branch_name),
        None => format!("ref {}", ref_name),
    }
}

fn describe_event(repo: &git2::Repository, event: &Event) -> anyhow::Result<Vec<StyledString>> {
    let render_commit = |oid: git2::Oid| -> anyhow::Result<StyledString> {
        match repo.find_commit(oid) {
            Ok(commit) => render_commit_metadata(
                &commit,
                &mut [
                    &mut CommitOidProvider::new(true)?,
                    &mut CommitMessageProvider::new()?,
                ],
            ),
            Err(_) => Ok(StyledString::plain(format!(
                "<unavailable: {} (possibly GC'ed)>",
                oid.to_string()
            ))),
        }
    };
    let result = match event {
        Event::CommitEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Commit ")
                    .append(render_commit(*commit_oid)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::HideEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Hide commit ")
                    .append(render_commit(*commit_oid)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::UnhideEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Unhide commit ")
                    .append(render_commit(*commit_oid)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: None,
            new_ref: Some(new_ref),
            message: _,
        } if ref_name == "HEAD" => {
            // Not sure if this can happen. When a repo is created, maybe?
            vec![
                StyledStringBuilder::new()
                    .append_plain("Check out to ")
                    .append(render_commit(new_ref.parse()?)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: Some(old_ref),
            new_ref: Some(new_ref),
            message: _,
        } if ref_name == "HEAD" => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Check out from ")
                    .append(render_commit(old_ref.parse()?)?)
                    .build(),
                StyledStringBuilder::new()
                    .append_plain("            to ")
                    .append(render_commit(new_ref.parse()?)?)
                    .build(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: None,
            new_ref: None,
            message: _,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Empty event for ")
                    .append_plain(render_ref_name(ref_name))
                    .build(),
                StyledStringBuilder::new()
                    .append_plain("This event should not appear. ")
                    .append_plain("This is a (benign) bug -- ")
                    .append_plain("please report it.")
                    .build(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: None,
            new_ref: Some(new_ref),
            message: _,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Create ")
                    .append_plain(render_ref_name(ref_name))
                    .append_plain(" at ")
                    .append(render_commit(new_ref.parse()?)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: Some(old_ref),
            new_ref: None,
            message: _,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Delete ")
                    .append_plain(render_ref_name(ref_name))
                    .append_plain(" at ")
                    .append(render_commit(old_ref.parse()?)?)
                    .build(),
                StyledString::new(),
            ]
        }

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref: Some(old_ref),
            new_ref: Some(new_ref),
            message: _,
        } => {
            let ref_name = render_ref_name(ref_name);
            vec![
                StyledStringBuilder::new()
                    .append_plain("Move ")
                    .append_plain(ref_name.clone())
                    .append_plain(" from ")
                    .append(render_commit(old_ref.parse()?)?)
                    .build(),
                StyledStringBuilder::new()
                    .append_plain("     ")
                    .append_plain(" ".repeat(ref_name.len()))
                    .append_plain("   to ")
                    .append(render_commit(new_ref.parse()?)?)
                    .build(),
            ]
        }

        Event::RewriteEvent {
            timestamp: _,
            event_tx_id: _,
            old_commit_oid,
            new_commit_oid,
        } => {
            vec![
                StyledStringBuilder::new()
                    .append_plain("Rewrite commit ")
                    .append(render_commit(*old_commit_oid)?)
                    .build(),
                StyledStringBuilder::new()
                    .append_plain("           as ")
                    .append(render_commit(*new_commit_oid)?)
                    .build(),
            ]
        }
    };
    Ok(result)
}

fn describe_events_numbered(
    repo: &git2::Repository,
    events: &[Event],
) -> Result<Vec<StyledString>, anyhow::Error> {
    let mut lines = Vec::new();
    for (i, event) in (1..).zip(events) {
        let num_header = format!("{}. ", i);
        for (j, event_line) in (0..).zip(describe_event(&repo, &event)?) {
            let prefix = if j == 0 {
                num_header.clone()
            } else {
                " ".repeat(num_header.len())
            };
            lines.push(
                StyledStringBuilder::new()
                    .append_plain(prefix)
                    .append(event_line)
                    .build(),
            );
        }
    }
    Ok(lines)
}

fn select_past_event(
    mut siv: CursiveRunner<CursiveRunnable>,
    glyphs: &Glyphs,
    repo: &git2::Repository,
    merge_base_db: &MergeBaseDb,
    event_replayer: &mut EventReplayer,
) -> anyhow::Result<Option<EventCursor>> {
    #[derive(Clone, Copy, Debug)]
    enum Message {
        Init,
        Next,
        Previous,
        GoToEvent,
        SetEventReplayerCursor { event_id: isize },
        Help,
        Quit,
        SelectEventIdAndQuit,
    }
    let (main_tx, main_rx): (Sender<Message>, Receiver<Message>) = channel();

    [
        ('n'.into(), Message::Next),
        ('N'.into(), Message::Next),
        (Key::Right.into(), Message::Next),
        ('p'.into(), Message::Previous),
        ('P'.into(), Message::Previous),
        (Key::Left.into(), Message::Previous),
        ('h'.into(), Message::Help),
        ('H'.into(), Message::Help),
        ('?'.into(), Message::Help),
        ('g'.into(), Message::GoToEvent),
        ('G'.into(), Message::GoToEvent),
        ('q'.into(), Message::Quit),
        ('Q'.into(), Message::Quit),
        (
            cursive::event::Key::Enter.into(),
            Message::SelectEventIdAndQuit,
        ),
    ]
    .iter()
    .cloned()
    .for_each(|(event, message): (cursive::event::Event, Message)| {
        siv.add_global_callback(event, {
            let main_tx = main_tx.clone();
            move |_siv| main_tx.send(message).unwrap()
        });
    });

    let mut cursor = event_replayer.make_default_cursor();
    let now = SystemTime::now();
    main_tx.send(Message::Init)?;
    while siv.is_running() {
        let message = main_rx.try_recv();
        if message.is_err() {
            // For tests: only pump the Cursive event loop if we have no events
            // of our own to process. Otherwise, the event loop queues up all of
            // the messages before we can process them, which means that none of
            // the screenshots are correct.
            siv.step();
        }

        declare_views! {
            SmartlogView => ScrollView<TextView>,
            InfoView => TextView,
        }

        let redraw = |siv: &mut Cursive,
                      event_replayer: &mut EventReplayer,
                      event_cursor: EventCursor|
         -> anyhow::Result<()> {
            let smartlog = render_cursor_smartlog(
                &glyphs,
                &repo,
                &merge_base_db,
                &event_replayer,
                event_cursor,
            )?;
            SmartlogView::find(siv)
                .get_inner_mut()
                .set_content(StyledStringBuilder::from_lines(smartlog));

            let event = event_replayer.get_tx_events_before_cursor(event_cursor);
            let info_view_contents = match event {
                None => vec![StyledString::plain(
                    "There are no previous available events.",
                )],
                Some((event_id, events)) => {
                    let event_description = {
                        let lines = describe_events_numbered(repo, &events)?;
                        StyledStringBuilder::from_lines(lines)
                    };
                    let relative_time_provider = RelativeTimeProvider::new(repo, now)?;
                    let relative_time = if relative_time_provider.is_enabled() {
                        format!(
                            " ({} ago)",
                            RelativeTimeProvider::describe_time_delta(
                                now,
                                events[0].get_timestamp()
                            )?
                        )
                    } else {
                        String::new()
                    };
                    vec![
                        StyledStringBuilder::new()
                            .append_plain("Repo after transaction ")
                            .append_plain(events[0].get_event_tx_id().to_string())
                            .append_plain(" (event ")
                            .append_plain(event_id.to_string())
                            .append_plain(")")
                            .append_plain(relative_time)
                            .append_plain(". Press 'h' for help, 'q' to quit.")
                            .build(),
                        event_description,
                    ]
                }
            };
            InfoView::find(siv).set_content(StyledStringBuilder::from_lines(info_view_contents));
            Ok(())
        };

        match message {
            Err(TryRecvError::Disconnected) => break,

            Err(TryRecvError::Empty) => {
                // If we haven't received a message yet, defer to `siv.step`
                // to process the next user input.
                continue;
            }

            Ok(Message::Init) => {
                let smartlog_view: SmartlogView = ScrollView::new(TextView::new("")).into();
                let info_view: InfoView = TextView::new("").into();
                siv.add_layer(
                    LinearLayout::vertical()
                        .child(smartlog_view)
                        .child(info_view),
                );
                redraw(&mut siv, event_replayer, cursor)?;
            }

            Ok(Message::Next) => {
                cursor = event_replayer.advance_cursor_by_transaction(cursor, 1);
                redraw(&mut siv, event_replayer, cursor)?;
            }

            Ok(Message::Previous) => {
                cursor = event_replayer.advance_cursor_by_transaction(cursor, -1);
                redraw(&mut siv, event_replayer, cursor)?;
            }

            Ok(Message::SetEventReplayerCursor { event_id }) => {
                cursor = event_replayer.make_cursor(event_id);
                redraw(&mut siv, event_replayer, cursor)?;
            }

            Ok(Message::GoToEvent) => {
                let main_tx = main_tx.clone();
                siv.add_layer(
                    OnEventView::new(
                        Dialog::new()
                            .title("Go to event")
                            .content(EditView::new().on_submit(move |siv, text| {
                                match text.parse::<isize>() {
                                    Ok(event_id) => {
                                        main_tx
                                            .send(Message::SetEventReplayerCursor { event_id })
                                            .unwrap();
                                        siv.pop_layer();
                                    }
                                    Err(_) => {
                                        siv.add_layer(Dialog::info(format!(
                                            "Invalid event ID: {}",
                                            text
                                        )));
                                    }
                                }
                            }))
                            .dismiss_button("Cancel"),
                    )
                    .on_event(Key::Esc, |siv| {
                        siv.pop_layer();
                    }),
                );
            }

            Ok(Message::Help) => {
                siv.add_layer(
                        Dialog::new()
                            .title("How to use")
                            .content(TextView::new(
"Use `git undo` to view and revert to previous states of the repository.

h/?: Show this help.
q: Quit.
p/n or <left>/<right>: View next/previous state.
g: Go to a provided event ID.
<enter>: Revert the repository to the given state (requires confirmation).

You can also copy a commit hash from the past and manually run `git unhide` or `git rebase` on it.
",
                            ))
                            .dismiss_button("Close"),
                    );
            }

            Ok(Message::Quit) => siv.quit(),

            Ok(Message::SelectEventIdAndQuit) => {
                siv.quit();
                return Ok(Some(cursor));
            }
        };

        if message.is_ok() {
            siv.refresh();
        }
    }

    Ok(None)
}

fn inverse_event(
    event: Event,
    now: SystemTime,
    event_tx_id: EventTransactionId,
) -> anyhow::Result<Event> {
    let timestamp = now.duration_since(SystemTime::UNIX_EPOCH)?.as_secs_f64();
    let inverse_event = match event {
        Event::CommitEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        }
        | Event::UnhideEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        } => Event::HideEvent {
            timestamp,
            event_tx_id,
            commit_oid,
        },

        Event::HideEvent {
            timestamp: _,
            event_tx_id: _,
            commit_oid,
        } => Event::UnhideEvent {
            timestamp,
            event_tx_id,
            commit_oid,
        },

        Event::RewriteEvent {
            timestamp: _,
            event_tx_id: _,
            old_commit_oid,
            new_commit_oid,
        } => Event::RewriteEvent {
            timestamp,
            event_tx_id,
            old_commit_oid: new_commit_oid,
            new_commit_oid: old_commit_oid,
        },

        Event::RefUpdateEvent {
            timestamp: _,
            event_tx_id: _,
            ref_name,
            old_ref,
            new_ref,
            message: _,
        } => Event::RefUpdateEvent {
            timestamp,
            event_tx_id,
            ref_name,
            old_ref: new_ref,
            new_ref: old_ref,
            message: None,
        },
    };
    Ok(inverse_event)
}

fn optimize_inverse_events(events: Vec<Event>) -> Vec<Event> {
    let mut optimized_events = Vec::new();
    let mut seen_checkout = false;
    for event in events.into_iter().rev() {
        match event {
            Event::RefUpdateEvent { ref ref_name, .. } if ref_name == "HEAD" => {
                if seen_checkout {
                    continue;
                } else {
                    seen_checkout = true;
                    optimized_events.push(event)
                }
            }
            event => optimized_events.push(event),
        };
    }
    optimized_events.reverse();
    optimized_events
}

fn undo_events(
    in_: &mut impl Read,
    out: &mut impl Write,
    glyphs: &Glyphs,
    repo: &git2::Repository,
    git_executable: &GitExecutable,
    event_log_db: &mut EventLogDb,
    event_replayer: &EventReplayer,
    event_cursor: EventCursor,
) -> anyhow::Result<isize> {
    let now = SystemTime::now();
    let event_tx_id = event_log_db.make_transaction_id(now, "undo")?;
    let inverse_events: Vec<Event> = event_replayer
        .get_events_since_cursor(event_cursor)
        .iter()
        .rev()
        .filter(|event| {
            !matches!(
                event,
                Event::RefUpdateEvent {
                    timestamp: _,
                    event_tx_id: _,
                    ref_name,
                    old_ref: None,
                    new_ref: _,
                    message: _,
                } if ref_name == "HEAD"
            )
        })
        .map(|event| inverse_event(event.clone(), now, event_tx_id))
        .collect::<anyhow::Result<Vec<Event>>>()?;
    let mut inverse_events = optimize_inverse_events(inverse_events);

    // Move any checkout operations to be first. Otherwise, we have the risk
    // that `HEAD` is a symbolic reference pointing to another reference, and we
    // update that reference. This would cause the working copy to become dirty
    // from Git's perspective.
    inverse_events.sort_by_key(|event| match event {
        Event::RefUpdateEvent { ref_name, .. } if ref_name == "HEAD" => 0,
        _ => 1,
    });

    if inverse_events.is_empty() {
        writeln!(out, "No undo actions to apply, exiting.")?;
        return Ok(0);
    }

    writeln!(out, "Will apply these actions:")?;
    let events = describe_events_numbered(&repo, &inverse_events)?;
    for line in events {
        writeln!(out, "{}", printable_styled_string(&glyphs, line)?)?;
    }

    let confirmed = {
        write!(out, "Confirm? [yN] ")?;
        out.flush()?;
        let mut user_input = String::new();
        let mut reader = BufReader::new(in_);
        match reader.read_line(&mut user_input) {
            Ok(_size) => {
                let user_input = user_input.trim();
                user_input == "y" || user_input == "Y"
            }
            Err(_) => false,
        }
    };
    if !confirmed {
        writeln!(out, "Aborted.")?;
        return Ok(1);
    }

    let num_inverse_events = Pluralize {
        amount: inverse_events.len().try_into().unwrap(),
        singular: "inverse event",
        plural: "inverse events",
    }
    .to_string();

    for event in inverse_events.into_iter() {
        match event {
            Event::RefUpdateEvent {
                timestamp: _,
                event_tx_id: _,
                ref_name,
                old_ref: _,
                new_ref: Some(new_ref),
                message: _,
            } if ref_name == "HEAD" => {
                // Most likely the user wanted to perform an actual checkout in
                // this case, rather than just update `HEAD` (and be left with a
                // dirty working copy). The `Git` command will update the event
                // log appropriately, as it will invoke our hooks.
                run_git(
                    git_executable,
                    Some(event_tx_id),
                    &["checkout", "--detach", &new_ref],
                )
                .with_context(|| "Updating to previous HEAD location")?;
            }
            Event::RefUpdateEvent {
                timestamp: _,
                event_tx_id: _,
                ref_name: _,
                old_ref: None,
                new_ref: None,
                message: _,
            } => {
                // Do nothing.
            }
            Event::RefUpdateEvent {
                timestamp: _,
                event_tx_id: _,
                ref_name,
                old_ref: Some(_),
                new_ref: None,
                message: _,
            } => match repo.find_reference(&ref_name) {
                Ok(mut reference) => {
                    reference
                        .delete()
                        .with_context(|| format!("Deleting reference: {}", ref_name))?;
                }
                Err(_) => {
                    writeln!(
                        out,
                        "Reference {} did not exist, not deleting it.",
                        ref_name
                    )?;
                }
            },
            Event::RefUpdateEvent {
                timestamp: _,
                event_tx_id: _,
                ref_name,
                old_ref: None,
                new_ref: Some(new_ref),
                message: _,
            }
            | Event::RefUpdateEvent {
                timestamp: _,
                event_tx_id: _,
                ref_name,
                old_ref: Some(_),
                new_ref: Some(new_ref),
                message: _,
            } => {
                // Create or update the given reference.
                let new_ref = new_ref.parse()?;
                repo.reference(&ref_name, new_ref, true, "branchless undo")?;
            }
            Event::CommitEvent { .. }
            | Event::HideEvent { .. }
            | Event::UnhideEvent { .. }
            | Event::RewriteEvent { .. } => {
                event_log_db.add_events(vec![event])?;
            }
        }
    }

    writeln!(out, "Applied {}.", num_inverse_events)?;
    Ok(0)
}

/// Restore the repository to a previous state interactively.
pub fn undo(git_executable: &GitExecutable) -> anyhow::Result<isize> {
    let glyphs = Glyphs::detect();
    let repo = get_repo()?;
    let conn = get_db_conn(&repo)?;
    let merge_base_db = MergeBaseDb::new(&conn)?;
    let mut event_log_db = EventLogDb::new(&conn)?;
    let mut event_replayer = EventReplayer::from_event_log_db(&event_log_db)?;

    let event_cursor = {
        let result = with_siv(|siv| {
            select_past_event(siv, &glyphs, &repo, &merge_base_db, &mut event_replayer)
        })?;
        match result {
            Some(event_cursor) => event_cursor,
            None => return Ok(0),
        }
    };

    let result = undo_events(
        &mut stdin(),
        &mut stdout().lock(),
        &glyphs,
        &repo,
        &git_executable,
        &mut event_log_db,
        &event_replayer,
        event_cursor,
    )?;
    Ok(result)
}

#[allow(missing_docs)]
pub mod testing {
    use std::io::{Read, Write};

    use cursive::{CursiveRunnable, CursiveRunner};

    use crate::core::eventlog::{EventCursor, EventLogDb, EventReplayer};
    use crate::core::formatting::Glyphs;
    use crate::core::mergebase::MergeBaseDb;
    use crate::util::GitExecutable;

    pub fn select_past_event(
        siv: CursiveRunner<CursiveRunnable>,
        glyphs: &Glyphs,
        repo: &git2::Repository,
        merge_base_db: &MergeBaseDb,
        event_replayer: &mut EventReplayer,
    ) -> anyhow::Result<Option<EventCursor>> {
        super::select_past_event(siv, glyphs, repo, merge_base_db, event_replayer)
    }

    pub fn undo_events(
        in_: &mut impl Read,
        out: &mut impl Write,
        glyphs: &Glyphs,
        repo: &git2::Repository,
        git_executable: &GitExecutable,
        event_log_db: &mut EventLogDb,
        event_replayer: &EventReplayer,
        event_cursor: EventCursor,
    ) -> anyhow::Result<isize> {
        super::undo_events(
            in_,
            out,
            glyphs,
            repo,
            git_executable,
            event_log_db,
            event_replayer,
            event_cursor,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::eventlog::testing::make_dummy_transaction_id;

    #[test]
    fn test_optimize_inverse_events() -> anyhow::Result<()> {
        let event_tx_id = make_dummy_transaction_id(123);
        let input = vec![
            Event::RefUpdateEvent {
                timestamp: 1.0,
                event_tx_id,
                ref_name: "HEAD".to_owned(),
                old_ref: Some("1".parse()?),
                new_ref: Some("2".parse()?),
                message: None,
            },
            Event::RefUpdateEvent {
                timestamp: 2.0,
                event_tx_id,
                ref_name: "HEAD".to_owned(),
                old_ref: Some("1".parse()?),
                new_ref: Some("3".parse()?),
                message: None,
            },
        ];
        let expected = vec![Event::RefUpdateEvent {
            timestamp: 2.0,
            event_tx_id,
            ref_name: "HEAD".to_owned(),
            old_ref: Some("1".parse()?),
            new_ref: Some("3".parse()?),
            message: None,
        }];
        assert_eq!(optimize_inverse_events(input), expected);
        Ok(())
    }
}
