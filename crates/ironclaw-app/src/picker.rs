use crate::home::{self, FolderItem};
use crate::theme;
use gpui::{
    actions, div, prelude::*, px, App, Context, EventEmitter, FocusHandle, Focusable,
    PathPromptOptions, SharedString, Window,
};
use std::path::PathBuf;

actions!(picker, [OpenSelected, BrowseNative, GoUp, UseDefault]);

pub struct HomeChosen {
    pub folder: PathBuf,
}

pub struct FolderPicker {
    cwd: PathBuf,
    items: Vec<FolderItem>,
    error: Option<String>,
    focus_handle: FocusHandle,
}

impl EventEmitter<HomeChosen> for FolderPicker {}

impl FolderPicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut picker = Self {
            cwd: home::starting_folder(),
            items: Vec::new(),
            error: None,
            focus_handle: cx.focus_handle(),
        };
        picker.reload();
        picker
    }

    fn reload(&mut self) {
        match home::list_folder(&self.cwd) {
            Ok(items) => {
                self.items = items;
                self.error = None;
            }
            Err(err) => {
                self.items = Vec::new();
                self.error = Some(err);
            }
        }
    }

    fn enter(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.cwd = path;
        self.reload();
        cx.notify();
    }

    fn go_up(&mut self, _: &GoUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.cwd.parent() {
            self.enter(parent.to_path_buf(), cx);
        }
    }

    fn use_default(&mut self, _: &UseDefault, _: &mut Window, cx: &mut Context<Self>) {
        self.enter(home::default_config_folder(), cx);
    }

    fn open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_open(window, cx);
    }

    fn confirm_open(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match home::open_home(&self.cwd) {
            Ok(opened) => {
                cx.open_with_system(&opened.folder);
                cx.emit(HomeChosen {
                    folder: opened.folder,
                });
            }
            Err(err) => {
                self.error = Some(err);
                cx.notify();
            }
        }
    }

    fn browse_native(&mut self, _: &BrowseNative, _: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose config folder".into()),
        });
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.enter(path, cx)).ok();
                }
            }
            Ok(Err(err)) => {
                this.update(cx, |this, cx| {
                    this.error = Some(err.to_string());
                    cx.notify();
                })
                .ok();
            }
            _ => {}
        })
        .detach();
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| handler(this, window, cx)))
            .child(label)
    }

    fn row(&self, item: FolderItem, cx: &mut Context<Self>) -> impl IntoElement {
        let path = item.path.clone();
        let is_dir = item.is_dir;
        let is_config = item.name == home::CONFIG_FILE_NAME;
        let name = item.name.clone();
        let label = if is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        div()
            .id(SharedString::from(format!("item-{}", path.display())))
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .flex()
            .justify_between()
            .cursor_pointer()
            .hover(|style| style.bg(theme::panel_2()))
            .on_click(cx.listener(move |this, _, window, cx| {
                if is_dir {
                    this.enter(path.clone(), cx);
                } else if is_config {
                    if let Some(parent) = path.parent() {
                        this.cwd = parent.to_path_buf();
                        this.confirm_open(window, cx);
                    }
                }
            }))
            .child(
                div()
                    .text_color(if is_dir || is_config {
                        theme::text()
                    } else {
                        theme::muted()
                    })
                    .child(label),
            )
            .child(div().text_xs().text_color(theme::muted()).child(if is_dir {
                "folder"
            } else if is_config {
                "config"
            } else {
                "file"
            }))
    }
}

impl Render for FolderPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_config = home::config_file_in(&self.cwd).is_some();
        let items = self.items.clone();
        div()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family("Inter")
            .text_sm()
            .flex()
            .items_center()
            .justify_center()
            .key_context("FolderPicker")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::browse_native))
            .on_action(cx.listener(Self::go_up))
            .on_action(cx.listener(Self::use_default))
            .child(
                div()
                    .w(px(640.))
                    .max_h(px(720.))
                    .bg(theme::sidebar())
                    .border_1()
                    .border_color(theme::border())
                    .rounded_lg()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_5()
                            .py_4()
                            .border_b_1()
                            .border_color(theme::border())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_color(theme::muted()).child("IRONCLAW"))
                            .child(div().child("Open a config folder"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(
                                        "Pick the daemon home — the directory that contains ironclawd.toml.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .px_5()
                            .py_3()
                            .border_b_1()
                            .border_color(theme::border())
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(theme::panel())
                                    .child(self.cwd.display().to_string()),
                            )
                            .child(self.toolbar_button("up", "Up", cx, |this, _, cx| {
                                if let Some(parent) = this.cwd.parent() {
                                    this.enter(parent.to_path_buf(), cx);
                                }
                            }))
                            .child(self.toolbar_button(
                                "browse",
                                "Browse…",
                                cx,
                                |this, window, cx| {
                                    this.browse_native(&BrowseNative, window, cx);
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("folder-list")
                            .flex_1()
                            .min_h(px(280.))
                            .px_3()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .overflow_y_scroll()
                            .children(items.into_iter().map(|item| self.row(item, cx)))
                            .when(self.items.is_empty(), |el| {
                                el.child(
                                    div()
                                        .px_3()
                                        .py_8()
                                        .text_color(theme::muted())
                                        .child("This folder is empty."),
                                )
                            }),
                    )
                    .when_some(self.error.clone(), |el, error| {
                        el.child(
                            div()
                                .px_5()
                                .py_2()
                                .text_color(theme::danger())
                                .child(error),
                        )
                    })
                    .child(
                        div()
                            .px_5()
                            .py_4()
                            .border_t_1()
                            .border_color(theme::border())
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if has_config {
                                        theme::accent_2()
                                    } else {
                                        theme::muted()
                                    })
                                    .child(if has_config {
                                        "ironclawd.toml found"
                                    } else {
                                        "No ironclawd.toml in this folder yet"
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(self.toolbar_button(
                                        "default-home",
                                        "Default home",
                                        cx,
                                        |this, _, cx| {
                                            this.enter(home::default_config_folder(), cx);
                                        },
                                    ))
                                    .child(
                                        div()
                                            .id("open-folder")
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(theme::accent())
                                            .text_color(theme::text())
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.confirm_open(window, cx);
                                            }))
                                            .child("Open folder"),
                                    ),
                            ),
                    ),
            )
    }
}

impl Focusable for FolderPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn bind_picker_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("enter", OpenSelected, Some("FolderPicker")),
        KeyBinding::new("cmd-o", BrowseNative, Some("FolderPicker")),
        KeyBinding::new("ctrl-o", BrowseNative, Some("FolderPicker")),
        KeyBinding::new("backspace", GoUp, Some("FolderPicker")),
        KeyBinding::new("cmd-shift-h", UseDefault, Some("FolderPicker")),
        KeyBinding::new("ctrl-shift-h", UseDefault, Some("FolderPicker")),
    ]);
}
