//! Synthetic protocol decisions only. These tests make no claim about actual Windows event tuples.
#[path = "command_runner/windows_console/binding_policy.rs"]
mod binding_policy;
use binding_policy::{Actor, Binding, Registration, Ticket, Witness};

fn registration() -> Registration {
    Registration {
        hwnd: 42,
        nonce: 7,
        generation: 1,
        creator: Actor {
            pid: 100,
            process_birth: 200,
            tid: 300,
            thread_birth: 400,
        },
        generator: Actor {
            pid: 100,
            process_birth: 200,
            tid: 301,
            thread_birth: 401,
        },
    }
}
fn ticket() -> Ticket {
    Ticket {
        nonce: 7,
        generation: 1,
        command: 1,
        kind: 0x8002,
    }
}
fn witness() -> Witness {
    Witness {
        hwnd: 42,
        kind: 0x8002,
        callback_sequence: 8,
        generator: registration().generator,
        live_creator: Some(registration().creator),
    }
}

#[test]
fn registered_separate_generator_maps_live_then_explicitly_handed_off_destroy() {
    let r = registration();
    let mut b = Binding::register(r, r.creator).unwrap();
    b.map(ticket(), witness()).unwrap();
    b.confirm_destroyed(r.nonce, r.generation).unwrap();
    b.map(
        Ticket {
            command: 2,
            kind: 0x8001,
            ..ticket()
        },
        Witness {
            kind: 0x8001,
            callback_sequence: 9,
            generator: r.creator,
            live_creator: None,
            ..witness()
        },
    )
    .unwrap();
}

#[test]
fn stale_generation_unknown_hwnd_and_replayed_events_fail_closed() {
    let r = registration();
    for t in [
        Ticket { nonce: 8, ..ticket() },
        Ticket {
            generation: 2,
            ..ticket()
        },
        Ticket { command: 3, ..ticket() },
    ] {
        assert!(Binding::register(r, r.creator).unwrap().map(t, witness()).is_err());
    }
    assert!(
        Binding::register(r, r.creator)
            .unwrap()
            .map(ticket(), Witness { hwnd: 99, ..witness() })
            .is_err()
    );
    let mut b = Binding::register(r, r.creator).unwrap();
    b.map(ticket(), witness()).unwrap();
    assert!(b.map(ticket(), witness()).is_err());
}

#[test]
fn recycled_numeric_hwnd_does_not_transfer_the_cooperative_generation() {
    let r = Registration {
        generation: 2,
        ..registration()
    };
    assert!(
        Binding::register(r, r.creator)
            .unwrap()
            .map(ticket(), witness())
            .is_err()
    );
}

#[test]
fn wrong_thread_process_birth_or_thread_birth_never_attributes() {
    let r = registration();
    for generator in [
        Actor {
            tid: r.creator.tid,
            ..r.generator
        },
        Actor {
            thread_birth: 999,
            ..r.generator
        },
        Actor {
            process_birth: 999,
            ..r.generator
        },
        Actor {
            pid: 999,
            ..r.generator
        },
    ] {
        assert!(
            Binding::register(r, r.creator)
                .unwrap()
                .map(ticket(), Witness { generator, ..witness() })
                .is_err()
        );
    }
    assert!(Binding::register(r, r.generator).is_err());
}

#[test]
fn invalid_destroy_handoff_and_missing_live_metadata_stay_unknown() {
    let r = registration();
    assert!(
        Binding::register(r, r.creator)
            .unwrap()
            .map(
                ticket(),
                Witness {
                    live_creator: None,
                    ..witness()
                }
            )
            .is_err()
    );
    let mut b = Binding::register(r, r.creator).unwrap();
    assert!(b.confirm_destroyed(r.nonce + 1, r.generation).is_err());
    assert!(
        b.map(
            Ticket {
                kind: 0x8001,
                ..ticket()
            },
            Witness {
                kind: 0x8001,
                live_creator: None,
                ..witness()
            }
        )
        .is_err()
    );
}
