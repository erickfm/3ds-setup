//! Controllers, read natively (gilrs) rather than through the web view: WebKitGTK's gamepad
//! support varies between builds, and in Steam's Gaming Mode the controller is Steam's virtual
//! one. Button presses go to the page as "pad" events: { button, down }, where button is one of
//! a b x y start select lb rb up down left right (by position, so "a" is the bottom face button).
//! "pads" events carry how many controllers are connected.

use gilrs::{Axis, Button, EventType, Gilrs};
use serde::Serialize;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone)]
struct Press {
    button: &'static str,
    down: bool,
}

fn name(b: Button) -> Option<&'static str> {
    Some(match b {
        Button::South => "a",
        Button::East => "b",
        Button::West => "x",
        Button::North => "y",
        Button::Start => "start",
        Button::Select => "select",
        Button::LeftTrigger => "lb",
        Button::RightTrigger => "rb",
        Button::DPadUp => "up",
        Button::DPadDown => "down",
        Button::DPadLeft => "left",
        Button::DPadRight => "right",
        _ => return None,
    })
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        let Ok(mut gilrs) = Gilrs::new() else { return };
        let _ = app.emit("pads", gilrs.gamepads().count());
        // The left stick acts as a D-pad: a direction is pressed past 0.5 and let go under 0.3.
        let (mut stick_x, mut stick_y): (Option<&'static str>, Option<&'static str>) = (None, None);
        // A Steam Deck can show up twice (its own controls and Steam's virtual controller), so
        // the same press arriving twice at once counts once.
        let mut last: Option<(&'static str, bool, Instant)> = None;
        let mut send = |button: &'static str, down: bool| {
            let now = Instant::now();
            if last.is_some_and(|(b, d, t)| b == button && d == down && now - t < Duration::from_millis(40)) {
                return;
            }
            last = Some((button, down, now));
            let _ = app.emit("pad", Press { button, down });
        };
        loop {
            let Some(ev) = gilrs.next_event_blocking(Some(Duration::from_millis(500))) else { continue };
            match ev.event {
                EventType::ButtonPressed(b, _) => {
                    if let Some(n) = name(b) {
                        send(n, true);
                    }
                }
                EventType::ButtonReleased(b, _) => {
                    if let Some(n) = name(b) {
                        send(n, false);
                    }
                }
                EventType::AxisChanged(axis @ (Axis::LeftStickX | Axis::LeftStickY), v, _) => {
                    let (held, neg, pos) = if axis == Axis::LeftStickX {
                        (&mut stick_x, "left", "right")
                    } else {
                        (&mut stick_y, "down", "up") // up is positive
                    };
                    let toward = if v > 0.0 { pos } else { neg };
                    let now = if v.abs() > 0.5 || (v.abs() > 0.3 && *held == Some(toward)) {
                        Some(toward)
                    } else {
                        None
                    };
                    if now != *held {
                        if let Some(h) = *held {
                            send(h, false);
                        }
                        if let Some(n) = now {
                            send(n, true);
                        }
                        *held = now;
                    }
                }
                EventType::Connected | EventType::Disconnected => {
                    let _ = app.emit("pads", gilrs.gamepads().count());
                }
                _ => {}
            }
        }
    });
}
