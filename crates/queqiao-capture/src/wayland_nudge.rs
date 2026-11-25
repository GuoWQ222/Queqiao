//! A still screen's first picture on wlroots desktops (docs/LINUX.md). xdg-desktop-portal-wlr (0.5, Ubuntu 22.04)
//! asks the compositor for every frame "with damage", the first one too: on a screen where nothing changes the
//! ScreenCast stream sends nothing, and a session would wait for its first picture until something moved. This
//! damages the screen without changing it: a 1 x 1 fully transparent surface on the overlay layer of each output (the
//! layer shell: wlroots and KDE have it; GNOME does not, and needs none: it sends a first frame at once), which takes
//! no input, committed again at each poke. It goes once the first picture came.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    outputs: Vec<wl_output::WlOutput>,
    /// One per output: its surface, its layer surface, its buffer, and whether the compositor configured it.
    pieces: Vec<Piece>,
}

struct Piece {
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    buffer: wl_buffer::WlBuffer,
    configured: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_output" => state
                    .outputs
                    .push(registry.bind(name, version.min(3), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let piece = state.pieces.iter_mut().find(|piece| &piece.layer == layer);
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                if let Some(piece) = piece {
                    piece.configured = true;
                }
            }
            // The output went (or the compositor closed it): no more commits on it.
            zwlr_layer_surface_v1::Event::Closed => {
                if let Some(piece) = piece {
                    piece.configured = false;
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: wl_compositor::WlCompositor);
delegate_noop!(State: wl_region::WlRegion);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_output::WlOutput);

pub(crate) struct Nudge {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Nudge {
    /// The surfaces, one per output, mapped (they show nothing). An error where the compositor has no layer shell.
    pub(crate) fn new() -> Result<Nudge, String> {
        let connection = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland: {e}"))?;
        let (Some(compositor), Some(shm), Some(layer_shell)) = (
            state.compositor.clone(),
            state.shm.clone(),
            state.layer_shell.clone(),
        ) else {
            return Err("the compositor has no layer shell".into());
        };
        if state.outputs.is_empty() {
            return Err("the compositor has no outputs".into());
        }
        // One transparent pixel (A, R, G, B all 0), in memory shared with the compositor.
        let fd = unsafe { libc::memfd_create(c"queqiao-nudge".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(format!("memfd: {}", std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(std::os::fd::AsRawFd::as_raw_fd(&fd), 4) } != 0 {
            return Err(format!("memfd: {}", std::io::Error::last_os_error()));
        }
        let pool = shm.create_pool(fd.as_fd(), 4, &qh, ());
        for output in state.outputs.clone() {
            let surface = compositor.create_surface(&qh, ());
            // It takes no input: clicks and touches go to what is under it.
            let region = compositor.create_region(&qh, ());
            surface.set_input_region(Some(&region));
            region.destroy();
            let layer = layer_shell.get_layer_surface(
                &surface,
                Some(&output),
                zwlr_layer_shell_v1::Layer::Overlay,
                "queqiao".into(),
                &qh,
                (),
            );
            layer.set_size(1, 1);
            layer.set_anchor(
                zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left,
            );
            // The first commit has no buffer: the compositor answers with a configure.
            surface.commit();
            let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, &qh, ());
            state.pieces.push(Piece {
                surface,
                layer,
                buffer,
                configured: false,
            });
        }
        pool.destroy();
        queue
            .roundtrip(&mut state)
            .map_err(|e| format!("Wayland: {e}"))?;
        Ok(Nudge {
            connection,
            queue,
            state,
        })
    }

    /// Commits the pixel again on every output: the compositor draws, and counts that as a change.
    pub(crate) fn poke(&mut self) -> Result<(), String> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| format!("Wayland: {e}"))?;
        for piece in self.state.pieces.iter().filter(|piece| piece.configured) {
            piece.surface.attach(Some(&piece.buffer), 0, 0);
            piece.surface.damage(0, 0, 1, 1);
            piece.surface.commit();
        }
        self.connection
            .flush()
            .map_err(|e| format!("Wayland: {e}"))?;
        // Configures and errors that came meanwhile.
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|e| format!("Wayland: {e}"))
    }
}

impl Drop for Nudge {
    fn drop(&mut self) {
        for piece in &self.state.pieces {
            piece.layer.destroy();
            piece.surface.destroy();
            piece.buffer.destroy();
        }
        let _ = self.connection.flush();
    }
}
