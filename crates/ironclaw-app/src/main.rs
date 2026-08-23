mod chat;
mod client;
mod model;
mod text_input;
mod theme;
mod workspace;

use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};
use text_input::bind_text_input_keys;
use workspace::{bind_workspace_keys, Workspace};

fn main() {
    Application::new().run(|cx: &mut App| {
        bind_text_input_keys(cx);
        bind_workspace_keys(cx);
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Ironclaw".into()),
                    ..Default::default()
                }),
                focus: true,
                show: true,
                ..Default::default()
            },
            |_, cx| cx.new(|cx| Workspace::new(cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
