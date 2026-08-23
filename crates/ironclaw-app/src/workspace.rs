use crate::chat::{ChatSession, Incoming};
use crate::client::DaemonClient;
use crate::model::{
    a2a_assignments, active_task_count, metrics, now_ms, parse_a2a_uri, push_thread_message,
    skill_label, task_summary, ChatMessage, ChatRole, FarmAgent, Snapshot, WorkspaceView,
};
use crate::text_input::{Submitted, TextInput};
use crate::theme::{self, initials};
use farm::{Capability, FarmTask, TaskState};
use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, SharedString,
    Window,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

actions!(
    workspace,
    [Quit, NewTask, AskTeammate, ChangeHome, RevealHome]
);

pub enum WorkspaceEvent {
    ChangeHome,
}

enum Dialog {
    Hidden,
    NewTask {
        requester_idx: usize,
        capability_idx: usize,
        capabilities: Vec<Capability>,
        error: String,
    },
    AskTeammate {
        capability_idx: usize,
        assignments: Vec<Capability>,
        note: String,
        error: String,
    },
}

pub struct Workspace {
    home: PathBuf,
    client: DaemonClient,
    view: WorkspaceView,
    health: String,
    error: Option<String>,
    agents: Vec<FarmAgent>,
    tasks: Vec<FarmTask>,
    capabilities: Vec<Capability>,
    selected_task_id: Option<String>,
    selected_agent_id: Option<String>,
    threads: HashMap<String, Vec<ChatMessage>>,
    chat: Option<ChatSession>,
    chat_ready: bool,
    composer: Entity<TextInput>,
    request: Entity<TextInput>,
    dialog: Dialog,
    focus_handle: FocusHandle,
}

impl gpui::EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub fn open(home: crate::home::OpenedHome, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| TextInput::new(cx, "Message agent…"));
        let request = cx.new(|cx| TextInput::new(cx, "Describe the outcome and constraints."));
        cx.subscribe(&composer, |this, _input, submitted: &Submitted, cx| {
            this.send_chat(submitted.0.clone(), cx);
        })
        .detach();
        let mut workspace = Self {
            home: home.folder.clone(),
            client: DaemonClient::from_home(&home),
            view: WorkspaceView::Delivery,
            health: "connecting".into(),
            error: None,
            agents: Vec::new(),
            tasks: Vec::new(),
            capabilities: Vec::new(),
            selected_task_id: None,
            selected_agent_id: None,
            threads: HashMap::new(),
            chat: None,
            chat_ready: false,
            composer,
            request,
            dialog: Dialog::Hidden,
            focus_handle: cx.focus_handle(),
        };
        workspace.spawn_loops(cx);
        workspace.refresh(cx);
        workspace
    }

    fn spawn_loops(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            if this.update(cx, |this, cx| this.drain_chat(cx)).is_err() {
                break;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            let client = match this.read_with(cx, |this, _| this.client.clone()) {
                Ok(client) => client,
                Err(_) => break,
            };
            let snapshot: Result<crate::model::Snapshot, String> = cx
                .background_executor()
                .spawn(async move { client.refresh() })
                .await;
            if this
                .update(cx, |this, cx| this.apply_snapshot(snapshot, cx))
                .is_err()
            {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(3)).await;
        })
        .detach();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let snapshot: Result<crate::model::Snapshot, String> = cx
                .background_executor()
                .spawn(async move { client.refresh() })
                .await;
            this.update(cx, |this, cx| this.apply_snapshot(snapshot, cx))
                .ok();
        })
        .detach();
    }

    fn apply_snapshot(&mut self, snapshot: Result<Snapshot, String>, cx: &mut Context<Self>) {
        match snapshot {
            Ok(snapshot) => {
                self.health = snapshot.health;
                self.agents = snapshot.agents;
                self.tasks = snapshot.tasks;
                self.error = None;
            }
            Err(err) => {
                self.health = "offline".into();
                self.error = Some(err);
            }
        }
        cx.notify();
    }

    fn drain_chat(&mut self, cx: &mut Context<Self>) {
        let events = self
            .chat
            .as_ref()
            .map(ChatSession::drain)
            .unwrap_or_default();
        if events.is_empty() {
            return;
        }
        let agent_id = self.selected_agent_id.clone().unwrap_or_default();
        for event in events {
            match event {
                Incoming::Ready => {
                    self.chat_ready = true;
                    self.composer.update(cx, |input, _| {
                        input.placeholder = "Message agent…".into();
                    });
                }
                Incoming::AgentText(text) => {
                    self.push_message(&agent_id, ChatRole::Agent, text, None, cx)
                }
                Incoming::System(text) => {
                    self.push_message(&agent_id, ChatRole::System, text, None, cx)
                }
                Incoming::Closed(text) => {
                    self.chat_ready = false;
                    self.chat = None;
                    self.push_message(&agent_id, ChatRole::System, text, None, cx);
                }
            }
        }
        cx.notify();
    }

    fn push_message(
        &mut self,
        agent_id: &str,
        role: ChatRole,
        text: String,
        task_id: Option<String>,
        _cx: &mut Context<Self>,
    ) {
        if agent_id.is_empty() {
            return;
        }
        push_thread_message(
            &mut self.threads,
            agent_id,
            ChatMessage {
                id: format!("msg-{}", now_ms()),
                at_ms: now_ms(),
                role,
                text,
                task_id,
            },
        );
    }

    fn set_view(&mut self, view: WorkspaceView, cx: &mut Context<Self>) {
        if view != WorkspaceView::Chat {
            self.close_chat();
        }
        self.view = view;
        self.dialog = Dialog::Hidden;
        cx.notify();
    }

    fn close_chat(&mut self) {
        self.chat = None;
        self.chat_ready = false;
    }

    fn open_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        if self.selected_agent_id.as_deref() != Some(agent_id.as_str()) {
            self.close_chat();
        }
        self.selected_agent_id = Some(agent_id.clone());
        self.view = WorkspaceView::Chat;
        self.load_capabilities(agent_id.clone(), cx);
        self.connect_chat(agent_id, cx);
        cx.notify();
    }

    fn connect_chat(&mut self, agent_id: String, cx: &mut Context<Self>) {
        self.chat_ready = false;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let connected: Result<(String, String), String> = cx
                .background_executor()
                .spawn(async move {
                    let ticket = client.ws_ticket(&agent_id)?;
                    let url = client.websocket_url(&agent_id, &ticket.ticket)?;
                    Ok::<_, String>((agent_id, url))
                })
                .await;
            this.update(cx, |this, cx| match connected {
                Ok((agent_id, url))
                    if this.selected_agent_id.as_deref() == Some(agent_id.as_str()) =>
                {
                    match ChatSession::connect(url, agent_id.clone()) {
                        Ok(session) => this.chat = Some(session),
                        Err(err) => this.push_message(&agent_id, ChatRole::System, err, None, cx),
                    }
                    cx.notify();
                }
                Ok(_) => {}
                Err(err) => {
                    let agent_id = this.selected_agent_id.clone().unwrap_or_default();
                    this.push_message(&agent_id, ChatRole::System, err, None, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_capabilities(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<Vec<farm::Capability>, String> = cx
                .background_executor()
                .spawn(async move { client.capabilities(&agent_id) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(capabilities) => this.capabilities = capabilities,
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_chat(&mut self, prompt: String, cx: &mut Context<Self>) {
        let Some(agent_id) = self.selected_agent_id.clone() else {
            return;
        };
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        match self.chat.as_ref() {
            Some(session) if self.chat_ready => {
                if let Err(err) = session.send_prompt(prompt.clone()) {
                    self.push_message(&agent_id, ChatRole::System, err, None, cx);
                } else {
                    self.push_message(&agent_id, ChatRole::User, prompt, None, cx);
                    self.composer.update(cx, |input, cx| input.clear(cx));
                }
            }
            _ => self.push_message(
                &agent_id,
                ChatRole::System,
                "Still connecting to this agent's MicroVM.".into(),
                None,
                cx,
            ),
        }
        cx.notify();
    }

    fn inspect_task(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.selected_task_id = Some(task_id);
        self.view = WorkspaceView::Delivery;
        cx.notify();
    }

    fn open_new_task(&mut self, cx: &mut Context<Self>) {
        if self.agents.is_empty() {
            self.error = Some("No agents are registered.".into());
            cx.notify();
            return;
        }
        let requester = self.agents[0].id.clone();
        self.dialog = Dialog::NewTask {
            requester_idx: 0,
            capability_idx: 0,
            capabilities: Vec::new(),
            error: String::new(),
        };
        self.request.update(cx, |input, cx| input.clear(cx));
        self.load_task_capabilities(requester, cx);
        cx.notify();
    }

    fn load_task_capabilities(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<Vec<farm::Capability>, String> = cx
                .background_executor()
                .spawn(async move { client.capabilities(&agent_id) })
                .await;
            this.update(cx, |this, cx| {
                if let Dialog::NewTask {
                    capabilities,
                    capability_idx,
                    error,
                    ..
                } = &mut this.dialog
                {
                    match result {
                        Ok(items) => {
                            *capabilities = a2a_assignments(&items);
                            *capability_idx = 0;
                            error.clear();
                        }
                        Err(err) => *error = err,
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn submit_new_task(&mut self, cx: &mut Context<Self>) {
        let Dialog::NewTask {
            requester_idx,
            capability_idx,
            capabilities,
            ..
        } = &self.dialog
        else {
            return;
        };
        let Some(requester) = self.agents.get(*requester_idx) else {
            return;
        };
        let Some(capability) = capabilities.get(*capability_idx) else {
            if let Dialog::NewTask { error, .. } = &mut self.dialog {
                *error = "Choose an assignment.".into();
            }
            cx.notify();
            return;
        };
        let Some((assignee, skill)) = parse_a2a_uri(capability.uri.as_str()) else {
            return;
        };
        let requester_id = requester.id.clone();
        let request = self.request.read(cx).text();
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<farm::FarmTask, String> = cx
                .background_executor()
                .spawn(
                    async move { client.create_task(&requester_id, &assignee, &skill, &request) },
                )
                .await;
            this.update(cx, |this, cx| match result {
                Ok(task) => {
                    this.dialog = Dialog::Hidden;
                    this.selected_task_id = Some(task.id);
                    this.view = WorkspaceView::Delivery;
                    this.refresh(cx);
                }
                Err(err) => {
                    if let Dialog::NewTask { error, .. } = &mut this.dialog {
                        *error = err;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_ask_teammate(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.selected_agent() else {
            return;
        };
        let assignments = a2a_assignments(&self.capabilities);
        let note = if assignments.is_empty() {
            format!("{} has no authorized outgoing A2A routes.", agent.name)
        } else {
            format!(
                "{} can request information only through the authorized routes below.",
                agent.name
            )
        };
        self.dialog = Dialog::AskTeammate {
            capability_idx: 0,
            assignments,
            note,
            error: String::new(),
        };
        self.request.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    fn submit_ask_teammate(&mut self, cx: &mut Context<Self>) {
        let Dialog::AskTeammate {
            capability_idx,
            assignments,
            ..
        } = &self.dialog
        else {
            return;
        };
        let Some(capability) = assignments.get(*capability_idx) else {
            return;
        };
        let Some((assignee, skill)) = parse_a2a_uri(capability.uri.as_str()) else {
            return;
        };
        let Some(requester) = self.selected_agent_id.clone() else {
            return;
        };
        let question = self.request.read(cx).text();
        let target_name = self
            .agent_by_id(&assignee)
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| assignee.clone());
        self.push_message(
            &requester,
            ChatRole::User,
            format!("Ask {target_name}: {question}"),
            None,
            cx,
        );
        self.dialog = Dialog::Hidden;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<farm::FarmTask, String> = cx
                .background_executor()
                .spawn(async move { client.create_task(&requester, &assignee, &skill, &question) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(task) => {
                    let agent_id = this.selected_agent_id.clone().unwrap_or_default();
                    this.push_message(
                        &agent_id,
                        ChatRole::System,
                        format!("A2A request sent to {target_name}."),
                        Some(task.id),
                        cx,
                    );
                    this.refresh(cx);
                }
                Err(err) => {
                    let agent_id = this.selected_agent_id.clone().unwrap_or_default();
                    this.push_message(&agent_id, ChatRole::System, err, None, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn selected_agent(&self) -> Option<&FarmAgent> {
        self.selected_agent_id
            .as_deref()
            .and_then(|id| self.agent_by_id(id))
    }

    fn agent_by_id(&self, id: &str) -> Option<&FarmAgent> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    fn selected_task(&self) -> Option<&FarmTask> {
        self.selected_task_id
            .as_deref()
            .and_then(|id| self.tasks.iter().find(|task| task.id == id))
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    fn change_home(&mut self, _: &ChangeHome, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::ChangeHome);
    }

    fn reveal_home(&mut self, _: &RevealHome, _: &mut Window, cx: &mut Context<Self>) {
        cx.open_with_system(&self.home);
    }

    fn new_task_action(&mut self, _: &NewTask, _: &mut Window, cx: &mut Context<Self>) {
        self.open_new_task(cx);
    }

    fn ask_action(&mut self, _: &AskTeammate, _: &mut Window, cx: &mut Context<Self>) {
        self.open_ask_teammate(cx);
    }

    fn channel_button(
        &self,
        id: &'static str,
        label: &'static str,
        view: WorkspaceView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.view == view;
        div()
            .id(id)
            .w_full()
            .px_2()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .text_color(if active {
                theme::text()
            } else {
                theme::muted()
            })
            .bg(if active {
                theme::panel_2()
            } else {
                theme::transparent()
            })
            .hover(|style| style.bg(theme::panel_2()).text_color(theme::text()))
            .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
            .child(label)
    }

    fn avatar_mark(&self, name: &str, large: bool) -> impl IntoElement {
        let size = if large { px(52.) } else { px(34.) };
        div()
            .size(size)
            .rounded_md()
            .bg(theme::avatar_color(name))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text())
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .child(initials(name))
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let agent_count = self.agents.len();
        div()
            .w(px(260.))
            .h_full()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_5()
                    .border_b_1()
                    .border_color(theme::border())
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().text_xs().text_color(theme::muted()).child("IRONCLAW"))
                            .child(div().text_color(theme::text()).child("Engineering"))
                            .child(
                                div()
                                    .id("reveal-home")
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        cx.open_with_system(&this.home);
                                    }))
                                    .child(self.home.display().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::accent_2())
                                    .child(self.health.clone()),
                            )
                            .child(
                                div()
                                    .id("change-home")
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.change_home(&ChangeHome, window, cx);
                                    }))
                                    .child("Change folder"),
                            ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_5()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("CHANNELS"),
                    )
                    .child(self.channel_button(
                        "channel-chat",
                        "● conversations",
                        WorkspaceView::Chat,
                        cx,
                    ))
                    .child(self.channel_button(
                        "channel-delivery",
                        "# delivery",
                        WorkspaceView::Delivery,
                        cx,
                    ))
                    .child(self.channel_button("channel-team", "# team", WorkspaceView::Team, cx))
                    .child(self.channel_button(
                        "channel-architecture",
                        "# architecture",
                        WorkspaceView::Architecture,
                        cx,
                    )),
            )
            .child(
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!("AGENTS — {agent_count}")),
                    )
                    .children(self.agents.clone().into_iter().map(|agent| {
                        let selected = self.selected_agent_id.as_deref() == Some(agent.id.as_str())
                            && self.view == WorkspaceView::Chat;
                        let busy = active_task_count(&self.tasks, &agent.id) > 0;
                        let agent_id = agent.id.clone();
                        div()
                            .id(SharedString::from(format!("agent-{}", agent.id)))
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(if selected {
                                theme::panel_2()
                            } else {
                                theme::transparent()
                            })
                            .hover(|style| style.bg(theme::panel_2()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_agent(agent_id.clone(), cx)
                            }))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.avatar_mark(&agent.name, false))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .child(
                                        div()
                                            .text_color(theme::text())
                                            .text_sm()
                                            .child(agent.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::muted())
                                            .child(agent.role.clone()),
                                    ),
                            )
                            .child(div().size(px(10.)).rounded_full().bg(if busy {
                                theme::warning()
                            } else {
                                theme::accent_2()
                            }))
                    })),
            )
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = if self.view == WorkspaceView::Chat {
            self.selected_agent()
                .map(|agent| agent.name.clone())
                .unwrap_or_else(|| self.view.title().to_string())
        } else {
            self.view.title().to_string()
        };
        let description = if self.view == WorkspaceView::Chat {
            self.selected_agent()
                .map(|agent| {
                    format!(
                        "{} · {}",
                        agent.role,
                        if self.chat_ready {
                            "MicroVM connected"
                        } else {
                            "connecting to private MicroVM"
                        }
                    )
                })
                .unwrap_or_else(|| self.view.description().to_string())
        } else {
            self.view.description().to_string()
        };
        let chat_actions = self.view == WorkspaceView::Chat && self.selected_agent().is_some();
        div()
            .px_5()
            .h(px(77.))
            .border_b_1()
            .border_color(theme::border())
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_color(theme::text())
                            .child(format!("{} {title}", self.view.prefix())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(description),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(chat_actions, |el| {
                        el.child(self.action_button(
                            "ask-teammate",
                            "Ask teammate",
                            false,
                            cx,
                            |this, cx| this.open_ask_teammate(cx),
                        ))
                    })
                    .when(!chat_actions, |el| {
                        el.child(self.action_button(
                            "new-task",
                            "New task",
                            true,
                            cx,
                            |this, cx| this.open_new_task(cx),
                        ))
                    }),
            )
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        primary: bool,
        cx: &mut Context<Self>,
        handler: fn(&mut Self, &mut Context<Self>),
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .bg(if primary {
                theme::accent()
            } else {
                theme::panel_2()
            })
            .text_color(theme::text())
            .hover(|style| style.opacity(0.9))
            .on_click(cx.listener(move |this, _, _, cx| handler(this, cx)))
            .child(label)
    }

    fn render_metrics(&self) -> impl IntoElement {
        let counts = metrics(&self.agents, &self.tasks);
        div()
            .flex()
            .border_b_1()
            .border_color(theme::border())
            .children(
                [
                    ("active", counts.active.to_string()),
                    ("completed", counts.completed.to_string()),
                    ("needs attention", counts.failed.to_string()),
                    ("agents", counts.agents.to_string()),
                ]
                .into_iter()
                .map(|(label, value)| {
                    div()
                        .flex_1()
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .child(div().text_color(theme::text()).child(value))
                        .child(div().text_xs().text_color(theme::muted()).child(label))
                }),
            )
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .p_5()
            .id("main-scroll")
            .overflow_y_scroll()
            .child(match self.view {
                WorkspaceView::Delivery => self.render_delivery(cx).into_any_element(),
                WorkspaceView::Team => self.render_team(cx).into_any_element(),
                WorkspaceView::Architecture => self.render_architecture().into_any_element(),
                WorkspaceView::Chat => self.render_chat(cx).into_any_element(),
            })
    }

    fn render_delivery(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.error {
            return div()
                .p_8()
                .text_color(theme::danger())
                .child(error.clone())
                .into_any_element();
        }
        if self.tasks.is_empty() {
            return div()
                .p_8()
                .border_1()
                .border_color(theme::border())
                .rounded_lg()
                .text_color(theme::muted())
                .child("No work yet. Create the first team task.")
                .into_any_element();
        }
        let mut tasks = self.tasks.clone();
        tasks.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(tasks.into_iter().map(|task| {
                let selected = self.selected_task_id.as_deref() == Some(task.id.as_str());
                let task_id = task.id.clone();
                div()
                    .id(SharedString::from(format!("task-{}", task.id)))
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::border()
                    })
                    .bg(theme::panel())
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::accent()))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.inspect_task(task_id.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(self.state_pill(task.state))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(crate::model::format_timestamp(task.updated_at_ms)),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_color(theme::text())
                            .child(skill_label(&task.skill)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child(task_summary(&task)),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_xs()
                            .text_color(theme::muted())
                            .child(format!("{}  →  {}", task.requester, task.assignee)),
                    )
            }))
            .into_any_element()
    }

    fn state_pill(&self, state: TaskState) -> impl IntoElement {
        let (label, color) = match state {
            TaskState::Working | TaskState::Submitted | TaskState::InputRequired => {
                (format!("{state:?}").to_lowercase(), theme::warning())
            }
            TaskState::Completed => ("completed".into(), theme::accent_2()),
            TaskState::Failed | TaskState::Rejected => {
                (format!("{state:?}").to_lowercase(), theme::danger())
            }
            TaskState::Canceled => ("canceled".into(), theme::muted()),
        };
        div()
            .px_2()
            .py_1()
            .rounded_full()
            .text_xs()
            .text_color(color)
            .child(label)
    }

    fn render_team(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(self.agents.clone().into_iter().map(|agent| {
                let agent_id = agent.id.clone();
                div()
                    .id(SharedString::from(format!("person-{}", agent.id)))
                    .w(px(220.))
                    .p_5()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::accent()))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.open_agent(agent_id.clone(), cx)),
                    )
                    .child(self.avatar_mark(&agent.name, true))
                    .child(
                        div()
                            .mt_3()
                            .text_color(theme::text())
                            .child(agent.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::muted())
                            .child(agent.role.clone()),
                    )
                    .child(div().text_xs().text_color(theme::muted()).child(format!(
                        "{} skills · {} active tasks",
                        agent.a2a_skills,
                        active_task_count(&self.tasks, &agent.id)
                    )))
                    .child(
                        div()
                            .mt_4()
                            .text_xs()
                            .text_color(theme::accent_2())
                            .child("Open conversation →"),
                    )
            }))
    }

    fn render_architecture(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(self.arch_node("Telegram + Workspace", false))
            .child(div().text_color(theme::muted()).child("↓"))
            .child(self.arch_node(
                "Ironclaw host — registry · task ledger · MCP broker · VM manager",
                true,
            ))
            .child(
                div()
                    .text_color(theme::muted())
                    .child("↓ authenticated A2A tasks"),
            )
            .child(div().flex().flex_wrap().justify_center().gap_2().children(
                self.agents.iter().map(|agent| {
                    self.arch_node(
                        &format!("{} · {} · private VM", agent.name, agent.role),
                        false,
                    )
                }),
            ))
    }

    fn arch_node(&self, label: &str, host: bool) -> impl IntoElement {
        div()
            .px_4()
            .py_3()
            .rounded_md()
            .border_1()
            .border_color(if host {
                theme::accent()
            } else {
                theme::border()
            })
            .bg(theme::panel())
            .text_color(theme::text())
            .child(label.to_string())
    }

    fn render_chat(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(agent) = self.selected_agent().cloned() else {
            return div()
                .p_8()
                .text_color(theme::muted())
                .child("Select an agent from the sidebar to start a private conversation.")
                .into_any_element();
        };
        let messages = self.threads.get(&agent.id).cloned().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .h_full()
            .child(
                div()
                    .flex_1()
                    .id("chat-scroll")
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(messages.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .child(self.avatar_mark(&agent.name, true))
                                .child(
                                    div()
                                        .text_color(theme::text())
                                        .child(format!("Talk with {}", agent.name)),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::muted())
                                        .child("This conversation uses the agent's private MicroVM and memory."),
                                ),
                        )
                    })
                    .children(messages.into_iter().map(|message| {
                        let user = message.role == ChatRole::User;
                        div()
                            .id(SharedString::from(message.id.clone()))
                            .max_w(px(680.))
                            .when(user, |el| el.ml_auto())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(format!(
                                        "{}  {}",
                                        message.role.label(&agent.name),
                                        crate::model::format_timestamp(message.at_ms)
                                    )),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .p_3()
                                    .rounded_lg()
                                    .bg(if user { theme::user_bubble() } else { theme::panel() })
                                    .border_1()
                                    .border_color(theme::border())
                                    .text_color(theme::text())
                                    .child(message.text),
                            )
                    })),
            )
            .child(
                div()
                    .mt_3()
                    .p_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::panel())
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().px_2().child(self.composer.clone()))
                    .child(self.action_button(
                        "send-chat",
                        if self.chat_ready { "Send" } else { "Starting…" },
                        true,
                        cx,
                        |this, cx| {
                            let prompt = this.composer.read(cx).text();
                            this.send_chat(prompt, cx);
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_inspector(&self) -> impl IntoElement {
        let (title, rows, json_blocks): (String, Vec<(String, String)>, Vec<(String, String)>) =
            if let Some(task) = self.selected_task() {
                (
                    skill_label(&task.skill),
                    vec![
                        ("State".to_string(), format!("{:?}", task.state)),
                        ("Requester".to_string(), task.requester.clone()),
                        ("Assignee".to_string(), task.assignee.clone()),
                        ("Task".to_string(), task.id.clone()),
                        ("Context".to_string(), task.context_id.clone()),
                        ("Depth".to_string(), task.delegation_depth.to_string()),
                    ],
                    vec![
                        (
                            "Input".to_string(),
                            serde_json::to_string_pretty(&task.input).unwrap_or_default(),
                        ),
                        (
                            "Output".to_string(),
                            serde_json::to_string_pretty(&task.output)
                                .unwrap_or_else(|_| "null".into()),
                        ),
                    ],
                )
            } else if let Some(agent) = self.selected_agent() {
                (
                    agent.name.clone(),
                    vec![
                        ("Role".to_string(), agent.role.clone()),
                        ("Agent ID".to_string(), agent.id.clone()),
                        (
                            "Memory".to_string(),
                            format!("{} · Private VM", agent.memory_engine),
                        ),
                        (
                            "Active work".to_string(),
                            active_task_count(&self.tasks, &agent.id).to_string(),
                        ),
                        ("Wasm tools".to_string(), agent.wasm_tools.to_string()),
                        ("MCP servers".to_string(), agent.mcp_servers.to_string()),
                    ],
                    Vec::new(),
                )
            } else {
                (
                    "Select a task".into(),
                    vec![(
                        "Inspector".to_string(),
                        "Task input, delegation lineage, output, and artifacts appear here.".into(),
                    )],
                    Vec::new(),
                )
            };
        div()
            .w(px(320.))
            .h_full()
            .bg(theme::sidebar())
            .border_l_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_5()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::muted())
                            .child("INSPECTOR"),
                    )
                    .child(div().text_color(theme::text()).child(title)),
            )
            .child(
                div()
                    .p_5()
                    .id("inspector-scroll")
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows.into_iter().map(|(label, value)| {
                        div()
                            .py_2()
                            .border_b_1()
                            .border_color(theme::border())
                            .flex()
                            .justify_between()
                            .gap_3()
                            .child(div().text_color(theme::muted()).child(label))
                            .child(div().text_color(theme::text()).child(value))
                    }))
                    .children(json_blocks.into_iter().map(|(label, value)| {
                        div()
                            .mt_3()
                            .child(div().text_xs().text_color(theme::muted()).child(label))
                            .child(
                                div()
                                    .mt_1()
                                    .p_3()
                                    .rounded_md()
                                    .bg(theme::rail())
                                    .text_xs()
                                    .text_color(theme::muted())
                                    .child(value),
                            )
                    }))
                    .when(self.view == WorkspaceView::Chat, |el| {
                        el.child(
                            div()
                                .mt_3()
                                .text_xs()
                                .text_color(theme::muted())
                                .child("AUTHORIZED CAPABILITIES"),
                        )
                        .children(self.capabilities.iter().map(|capability| {
                            div()
                                .mt_1()
                                .p_2()
                                .rounded_md()
                                .bg(theme::rail())
                                .text_xs()
                                .text_color(theme::accent_2())
                                .child(capability.uri.as_str().to_string())
                        }))
                    }),
            )
    }

    fn render_dialog(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.dialog {
            Dialog::Hidden => div().into_any_element(),
            Dialog::NewTask {
                requester_idx,
                capability_idx,
                capabilities,
                error,
            } => {
                let requester_idx = *requester_idx;
                let capability_idx = *capability_idx;
                let requester_label = self
                    .agents
                    .get(requester_idx)
                    .map(|agent| format!("{} — {}", agent.name, agent.role))
                    .unwrap_or_else(|| "Choose agent".into());
                let capability_label = capabilities
                    .get(capability_idx)
                    .map(|capability| {
                        format!(
                            "{} — {}",
                            capability.uri.as_str().replace("agent://", ""),
                            capability.description
                        )
                    })
                    .unwrap_or_else(|| "Choose assignment".into());
                let request = self.request.clone();
                let requester_row = self.cycle_row(
                    "task-requester",
                    "Requesting agent",
                    requester_label,
                    cx,
                    |this, next, cx| {
                        if let Dialog::NewTask { requester_idx, .. } = &mut this.dialog {
                            let count = this.agents.len().max(1);
                            *requester_idx = if next {
                                (*requester_idx + 1) % count
                            } else {
                                (*requester_idx + count - 1) % count
                            };
                            if let Some(agent) = this.agents.get(*requester_idx) {
                                this.load_task_capabilities(agent.id.clone(), cx);
                            }
                        }
                    },
                );
                let capability_row = self.cycle_row(
                    "task-capability",
                    "Assignment",
                    capability_label,
                    cx,
                    |this, next, _cx| {
                        if let Dialog::NewTask {
                            capability_idx,
                            capabilities,
                            ..
                        } = &mut this.dialog
                        {
                            let count = capabilities.len().max(1);
                            *capability_idx = if next {
                                (*capability_idx + 1) % count
                            } else {
                                (*capability_idx + count - 1) % count
                            };
                        }
                    },
                );
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(requester_row)
                    .child(capability_row)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(theme::muted()).child("Request"))
                            .child(
                                div()
                                    .p_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg())
                                    .child(request),
                            ),
                    )
                    .into_any_element();
                self.modal(
                    "Create engineering task",
                    error.clone(),
                    cx,
                    body,
                    |this, cx| this.submit_new_task(cx),
                )
            }
            Dialog::AskTeammate {
                capability_idx,
                assignments,
                note,
                error,
            } => {
                let capability_idx = *capability_idx;
                let capability_label = assignments
                    .get(capability_idx)
                    .map(|capability| capability.uri.as_str().replace("agent://", ""))
                    .unwrap_or_else(|| "No routes".into());
                let note = note.clone();
                let request = self.request.clone();
                let capability_row = self.cycle_row(
                    "a2a-capability",
                    "Teammate and capability",
                    capability_label,
                    cx,
                    |this, next, _cx| {
                        if let Dialog::AskTeammate {
                            capability_idx,
                            assignments,
                            ..
                        } = &mut this.dialog
                        {
                            let count = assignments.len().max(1);
                            *capability_idx = if next {
                                (*capability_idx + 1) % count
                            } else {
                                (*capability_idx + count - 1) % count
                            };
                        }
                    },
                );
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::accent_2())
                            .text_sm()
                            .text_color(theme::text())
                            .child(note),
                    )
                    .child(capability_row)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(theme::muted()).child("Question"))
                            .child(
                                div()
                                    .p_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::bg())
                                    .child(request),
                            ),
                    )
                    .into_any_element();
                self.modal("Ask a teammate", error.clone(), cx, body, |this, cx| {
                    this.submit_ask_teammate(cx)
                })
            }
        }
    }

    fn cycle_row(
        &self,
        id: &'static str,
        label: &'static str,
        value: String,
        cx: &mut Context<Self>,
        handler: fn(&mut Self, bool, &mut Context<Self>),
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(theme::muted()).child(label))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-prev")))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme::panel_2())
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| handler(this, false, cx)))
                            .child("‹"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::bg())
                            .text_color(theme::text())
                            .child(value),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-next")))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme::panel_2())
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| handler(this, true, cx)))
                            .child("›"),
                    ),
            )
    }

    fn modal(
        &self,
        title: &'static str,
        error: String,
        cx: &mut Context<Self>,
        body: gpui::AnyElement,
        submit: fn(&mut Self, &mut Context<Self>),
    ) -> gpui::AnyElement {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0., 0., 0., 0.53))
            .child(
                div()
                    .w(px(520.))
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::sidebar())
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(div().text_color(theme::text()).child(title))
                            .child(
                                div()
                                    .id("close-dialog")
                                    .cursor_pointer()
                                    .text_color(theme::muted())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.dialog = Dialog::Hidden;
                                        cx.notify();
                                    }))
                                    .child("×"),
                            ),
                    )
                    .child(body)
                    .when(!error.is_empty(), |el| {
                        el.child(div().text_color(theme::danger()).child(error))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.action_button(
                                "dialog-cancel",
                                "Cancel",
                                false,
                                cx,
                                |this, cx| {
                                    this.dialog = Dialog::Hidden;
                                    cx.notify();
                                },
                            ))
                            .child(self.action_button("dialog-submit", "Submit", true, cx, submit)),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family("Inter")
            .text_sm()
            .flex()
            .key_context("Workspace")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::new_task_action))
            .on_action(cx.listener(Self::ask_action))
            .on_action(cx.listener(Self::change_home))
            .on_action(cx.listener(Self::reveal_home))
            .child(
                div()
                    .w(px(72.))
                    .h_full()
                    .bg(theme::rail())
                    .border_r_1()
                    .border_color(theme::border())
                    .flex()
                    .flex_col()
                    .items_center()
                    .py_4()
                    .gap_3()
                    .child(
                        div()
                            .size(px(46.))
                            .rounded_lg()
                            .bg(theme::accent())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::text())
                            .child("IC"),
                    )
                    .child(div().size(px(10.)).rounded_full().bg(
                        if self.health.contains("live") {
                            theme::accent_2()
                        } else {
                            theme::muted()
                        },
                    )),
            )
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(self.render_header(cx))
                    .child(self.render_metrics())
                    .child(self.render_content(cx)),
            )
            .child(self.render_inspector())
            .when(!matches!(self.dialog, Dialog::Hidden), |el| {
                el.child(self.render_dialog(cx))
            })
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn bind_workspace_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("cmd-n", NewTask, Some("Workspace")),
        KeyBinding::new("ctrl-n", NewTask, Some("Workspace")),
        KeyBinding::new("cmd-shift-o", ChangeHome, Some("Workspace")),
        KeyBinding::new("ctrl-shift-o", ChangeHome, Some("Workspace")),
        KeyBinding::new("cmd-shift-f", RevealHome, Some("Workspace")),
        KeyBinding::new("ctrl-shift-f", RevealHome, Some("Workspace")),
    ]);
}
