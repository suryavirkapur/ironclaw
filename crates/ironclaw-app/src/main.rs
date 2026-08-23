mod app;
mod chat;
mod client;
mod home;
mod model;
mod picker;
mod text_input;
mod theme;
mod workspace;

use app::{bind_app_keys, AppRoot};
use gpui::{px, size, App, AppContext, Application, Bounds, WindowBounds, WindowOptions};

fn main() {
    Application::new().run(|cx: &mut App| {
        bind_app_keys(cx);
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
            |_, cx| cx.new(AppRoot::new),
        )
        .unwrap();
        cx.activate(true);
    });
}
