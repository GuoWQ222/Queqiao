//! Drawing on this PC's screen (annotation, 2026-11-28, for help sessions): the viewer's pen strokes, shown to the
//! person here in a window over the shown screen (queqiao_display::DrawingOverlay) that never takes a click, a key or
//! the focus, and that the capture takes in (the viewer sees the strokes in the picture too). Also for someone
//! helping from another account: that is what it is for. The window goes when the viewer turns drawing off or the
//! session ends (this is dropped with it). Each step is answered with `Message::AnnotateState`, so the viewer knows
//! the drawing is shown (an agent before this never answers).

use queqiao_core::protocol::{Annotation, Message};
use queqiao_core::text::{tr, trf};
use queqiao_display::{DrawingArea, DrawingOverlay};
use queqiao_input::ScreenRect;
use queqiao_transport::Peer;

#[derive(Default)]
pub(crate) struct Drawing {
    overlay: Option<DrawingOverlay>,
    /// For the session's report: strokes finished, points that came, clears, and times drawing mode came on.
    strokes: u32,
    points: u32,
    clears: u32,
    turned_on: u32,
}

fn area(screen: ScreenRect) -> DrawingArea {
    DrawingArea {
        left: screen.left,
        top: screen.top,
        width: screen.width,
        height: screen.height,
    }
}

/// Why the drawing cannot be shown now, if it cannot: on Wayland (no window of Queqiao's can lie over everything there
/// and let clicks through), while this PC is locked or another desktop takes input (the sign-in, lock or UAC screen,
/// which no program's window covers).
pub(crate) fn refusal(adapter: &str, locked: bool, other_desktop: bool) -> Option<String> {
    if adapter == "Wayland" {
        Some(tr("Drawing on the screen is not possible on Wayland yet.").to_string())
    } else if locked {
        Some(tr("That PC is locked: drawing waits until it is unlocked.").to_string())
    } else if other_desktop {
        Some(
            tr("That PC shows its sign-in, lock or UAC screen: drawing waits until it is gone.")
                .to_string(),
        )
    } else {
        None
    }
}

impl Drawing {
    /// One step of the viewer's. `screen`: the screen shown now; `refused`: why drawing cannot be shown now.
    pub(crate) fn handle(
        &mut self,
        annotation: Annotation,
        screen: ScreenRect,
        refused: Option<String>,
        peer: &Peer,
        lines: &mut Vec<String>,
    ) {
        let answer = |shown: bool, strokes: usize, note: String| {
            let strokes = strokes.min(usize::from(u16::MAX)) as u16;
            peer.send_data(
                Message::AnnotateState {
                    shown,
                    strokes,
                    note,
                }
                .encode(),
            );
        };
        // Told on the steps that start something (not on each point of a stroke).
        let starts = matches!(annotation, Annotation::On | Annotation::Begin { .. });
        if annotation == Annotation::Off {
            if self.overlay.take().is_some() {
                lines.push("drawing: off, its window taken down".into());
            }
            answer(false, 0, String::new());
            return;
        }
        if let Some(why) = refused {
            if self.overlay.take().is_some() {
                lines.push(format!("drawing: taken down ({why})"));
            }
            if starts {
                lines.push(format!("drawing: refused: {why}"));
                answer(false, 0, why);
            }
            return;
        }
        if self.overlay.is_none() {
            let area = area(screen);
            match DrawingOverlay::show(area) {
                Ok(overlay) => {
                    lines.push(format!(
                        "drawing: shown over the screen ({}x{} at {}, {}), above everything, clicks passing through",
                        area.width, area.height, area.left, area.top
                    ));
                    self.overlay = Some(overlay);
                }
                Err(error) => {
                    if starts {
                        lines.push(format!("drawing: cannot be shown: {error}"));
                        let note = trf(
                            "Drawing cannot be shown on that PC's screen: {error}",
                            &[("error", &error)],
                        );
                        answer(false, 0, note);
                    }
                    return;
                }
            }
        }
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        overlay.apply(annotation);
        match annotation {
            Annotation::On => {
                self.turned_on += 1;
                answer(true, overlay.strokes(), String::new());
            }
            Annotation::Point { .. } => self.points += 1,
            Annotation::End => {
                self.strokes += 1;
                lines.push(format!(
                    "drawing: stroke {} shown ({} on the screen, {} points so far)",
                    self.strokes,
                    overlay.strokes(),
                    self.points
                ));
                answer(true, overlay.strokes(), String::new());
            }
            Annotation::Clear => {
                self.clears += 1;
                lines.push("drawing: cleared".into());
                answer(true, overlay.strokes(), String::new());
            }
            Annotation::Begin { .. } | Annotation::Off => {}
        }
    }

    /// The shown screen now: the drawing follows it (another screen shown, or its resolution changed).
    pub(crate) fn follow(&mut self, screen: ScreenRect, lines: &mut Vec<String>) {
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        let area = area(screen);
        if overlay.area() != area {
            overlay.move_to(area);
            lines.push(format!(
                "drawing: moved with the shown screen ({}x{} at {}, {})",
                area.width, area.height, area.left, area.top
            ));
        }
    }

    /// The report's line, if the viewer drew at all.
    pub(crate) fn report(&self) -> Option<String> {
        (self.turned_on > 0 || self.strokes > 0).then(|| {
            format!(
                "drawing: on {} times, {} strokes ({} points), {} clears",
                self.turned_on, self.strokes, self.points, self.clears
            )
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn drawing_is_refused_where_it_cannot_show() {
        assert!(super::refusal("Wayland", false, false).is_some());
        assert!(super::refusal("NVIDIA GeForce", true, false).is_some());
        assert!(super::refusal("X11", false, true).is_some());
        assert_eq!(super::refusal("X11", false, false), None);
    }
}
