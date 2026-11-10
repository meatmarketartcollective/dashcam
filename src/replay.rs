use crate::event::Event;

/// Terminal step-through replay: walk the timeline one hop at a time, with
/// the state at each hop (every summary up to and including it) so a failed
/// run reads like a story. The interactive loop is thin I/O in main; the
/// walking and rendering live here so they are testable.
pub struct Replay {
    events: Vec<Event>,
    cursor: usize,
}

impl Replay {
    pub fn new(events: Vec<Event>) -> Self {
        Self { events, cursor: 0 }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn position(&self) -> usize {
        self.cursor
    }

    /// Advance one hop. False already at the last hop.
    pub fn next(&mut self) -> bool {
        if self.cursor + 1 >= self.events.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    /// Step back one hop. False already at the first hop.
    pub fn back(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn first(&mut self) {
        self.cursor = 0;
    }

    pub fn last(&mut self) {
        if !self.events.is_empty() {
            self.cursor = self.events.len() - 1;
        }
    }

    pub fn current(&self) -> &Event {
        &self.events[self.cursor]
    }

    /// The full step display: hop header, pretty payload, and every summary
    /// up to and including this hop as the state at this point.
    pub fn render_current(&self) -> String {
        let e = self.current();
        let mut out = String::new();
        out.push_str(&format!(
            "hop {} of {} - {:?} - {}\n{}\n",
            e.seq,
            self.events.len(),
            e.kind,
            e.summary,
            e.ts
        ));
        if e.payload.len() > 2 && e.payload != "{}" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                    out.push_str(&format!("\n{pretty}\n"));
                } else {
                    out.push_str(&format!("\n{}\n", e.payload));
                }
            } else {
                out.push_str(&format!("\n{}\n", e.payload));
            }
        }
        out.push_str("\nstate at this hop:\n");
        for (i, ev) in self.events[..=self.cursor].iter().enumerate() {
            out.push_str(&format!("  {}. {:?} {}\n", i + 1, ev.kind, ev.summary));
        }
        out
    }
}
