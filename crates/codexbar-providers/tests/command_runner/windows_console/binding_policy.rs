//! Cooperative fixture protocol only. A WinEvent does NOT carry this generation/command witness.
//! No caller may use this policy to attribute an unregistered provider or broker window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    pub pid: u32,
    pub process_birth: u64,
    pub tid: u32,
    pub thread_birth: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Registration {
    pub hwnd: u64,
    pub nonce: u64,
    pub generation: u64,
    pub creator: Actor,
    pub generator: Actor,
}
#[derive(Clone, Copy, Debug)]
pub struct Ticket {
    pub nonce: u64,
    pub generation: u64,
    pub command: u64,
    pub kind: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct Witness {
    pub hwnd: u64,
    pub kind: u32,
    pub callback_sequence: u64,
    pub generator: Actor, // Read back from the retained live thread, not guessed from a TID.
    pub live_creator: Option<Actor>,
}
pub struct Binding {
    registration: Registration,
    destroyed: bool,
    last_command: u64,
    last_callback: u64,
}
impl Binding {
    pub fn register(registration: Registration, readback: Actor) -> Result<Self, &'static str> {
        let Registration {
            hwnd,
            nonce,
            generation,
            creator,
            generator,
        } = registration;
        if hwnd == 0 || nonce == 0 || generation == 0 || creator != readback {
            return Err("invalid-window-registration");
        }
        if [creator, generator]
            .iter()
            .any(|a| a.pid == 0 || a.process_birth == 0 || a.tid == 0 || a.thread_birth == 0)
            || creator.pid != generator.pid
            || creator.process_birth != generator.process_birth
            || creator.tid == generator.tid
        {
            return Err("invalid-retained-actors");
        }
        Ok(Self {
            registration,
            destroyed: false,
            last_command: 0,
            last_callback: 0,
        })
    }
    pub fn confirm_destroyed(&mut self, nonce: u64, generation: u64) -> Result<(), &'static str> {
        if nonce != self.registration.nonce || generation != self.registration.generation || self.destroyed {
            return Err("invalid-cooperative-destroy-handoff");
        }
        self.destroyed = true;
        Ok(())
    }
    pub fn map(&mut self, ticket: Ticket, witness: Witness) -> Result<(), &'static str> {
        let r = self.registration;
        if ticket.nonce != r.nonce || ticket.generation != r.generation || witness.hwnd != r.hwnd {
            return Err("unknown-or-stale-generation");
        }
        // The separate owned thread announces SHOW; the creator performs the real DestroyWindow.
        let expected_generator = if witness.kind == 0x8001 { r.creator } else { r.generator };
        if witness.generator != expected_generator {
            return Err("wrong-generator-identity");
        }
        if ticket.command != self.last_command + 1
            || witness.callback_sequence <= self.last_callback
            || ticket.kind != witness.kind
        {
            return Err("missing-or-replayed-command-event");
        }
        match witness.kind {
            0x8002 if !self.destroyed && witness.live_creator == Some(r.creator) => {}
            0x8001 if self.destroyed && witness.live_creator.is_none() => {}
            _ => return Err("missing-live-or-destroy-handoff-metadata"),
        }
        self.last_command = ticket.command;
        self.last_callback = witness.callback_sequence;
        Ok(())
    }
}
