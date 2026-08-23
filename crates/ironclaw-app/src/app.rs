use crate::picker::{bind_picker_keys, FolderPicker, HomeChosen};
use crate::text_input::bind_text_input_keys;
use crate::workspace::{bind_workspace_keys, Workspace, WorkspaceEvent};
use gpui::{div, prelude::*, App, Context, Entity, Window};

pub struct AppRoot {
    picker: Entity<FolderPicker>,
    workspace: Option<Entity<Workspace>>,
}

impl AppRoot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let picker = cx.new(FolderPicker::new);
        cx.subscribe(&picker, |this, _picker, event: &HomeChosen, cx| {
            this.open_home(event.folder.clone(), cx);
        })
        .detach();
        Self {
            picker,
            workspace: None,
        }
    }

    fn open_home(&mut self, folder: std::path::PathBuf, cx: &mut Context<Self>) {
        match crate::home::open_home(&folder) {
            Ok(opened) => {
                let workspace = cx.new(|cx| Workspace::open(opened, cx));
                cx.subscribe(
                    &workspace,
                    |this, _ws, event: &WorkspaceEvent, cx| match event {
                        WorkspaceEvent::ChangeHome => {
                            this.workspace = None;
                            cx.notify();
                        }
                    },
                )
                .detach();
                self.workspace = Some(workspace);
                cx.notify();
            }
            Err(_) => {
                self.workspace = None;
                cx.notify();
            }
        }
    }
}

impl Render for AppRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(match &self.workspace {
            Some(workspace) => workspace.clone().into_any_element(),
            None => self.picker.clone().into_any_element(),
        })
    }
}

pub fn bind_app_keys(cx: &mut App) {
    bind_text_input_keys(cx);
    bind_picker_keys(cx);
    bind_workspace_keys(cx);
}
