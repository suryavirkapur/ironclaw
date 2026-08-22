//! Ironclaw Workspace — a native GPUI desktop client for the ironclawd agent
//! farm. It mirrors the web workspace UI (rail, sidebar, delivery/team/
//! architecture/conversation views, live metrics, inspector, and the New Task
//! dialog) while talking to the same `/api/farm/*` control-plane REST API.

mod client;
mod models;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    div, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba, size, App,
    Background, Bounds, BoxShadow, Context, FocusHandle, FontWeight, KeyDownEvent, Rgba, Task,
    Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

use client::{Shared, Snapshot};
use models::{AgentSummary, Capability, FarmTask, TaskState};

// ---------------------------------------------------------------------------
// Palette (matches ui/app.css)
// ---------------------------------------------------------------------------
const RAIL: u32 = 0x090b0f;
const PANEL: u32 = 0x1e222b;
const PANEL2: u32 = 0x242934;
const TEXT: u32 = 0xf4f6fb;
const MUTED: u32 = 0x9199aa;
const BORDER: u32 = 0x303642;
const ACCENT: u32 = 0x8b7cf6;
const ACCENT2: u32 = 0x55d6be;
const DANGER: u32 = 0xf26b76;
const WARNING: u32 = 0xe9b44c;

// -- visual helpers ---------------------------------------------------------

/// A diagonal two-stop gradient background.
fn grad(from: u32, to: u32, angle: f32) -> Background {
    linear_gradient(
        angle,
        linear_color_stop(rgb(from), 0.),
        linear_color_stop(rgb(to), 1.),
    )
}

/// The avatar gradient used across the workspace.
fn avatar_grad() -> Background {
    grad(0x6b5cf0, 0x2fb8a8, 145.)
}

/// Soft elevation shadow for cards.
fn shadow_soft() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(6.), rgba(0x00000055).into())
        .blur_radius(px(18.))
        .spread_radius(px(-6.))]
}

/// Pronounced elevation shadow for floating surfaces (dialog).
fn shadow_deep() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(30.), rgba(0x000000cc).into())
        .blur_radius(px(80.))
        .spread_radius(px(-10.))]
}

/// A soft colored glow (e.g. presence / status dots, accent buttons).
fn glow(argb: u32, blur: f32) -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), rgba(argb).into()).blur_radius(px(blur))]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Conversations,
    Delivery,
    Team,
    Architecture,
}

#[derive(Clone)]
struct ChatMsg {
    role: &'static str,
    text: String,
}

struct Workspace {
    shared: Shared,
    base_url: String,
    view: View,
    selected_task: Option<String>,
    selected_agent: Option<String>,
    focus: FocusHandle,
    focused_once: bool,
    // New Task dialog state
    dialog_open: bool,
    dialog_requester: usize,
    dialog_caps: Vec<Capability>,
    dialog_cap: usize,
    dialog_request: String,
    dialog_error: Option<String>,
    // conversation composer + local threads
    composer_text: String,
    threads: HashMap<String, Vec<ChatMsg>>,
    // per-agent authorized capabilities, fetched lazily for the inspector
    caps: Arc<Mutex<HashMap<String, Vec<Capability>>>>,
    _refresh: Task<()>,
}

impl Workspace {
    fn new(base_url: String, cx: &mut Context<Self>) -> Self {
        let shared: Shared = Arc::new(Mutex::new(Snapshot::default()));
        client::spawn_poller(base_url.clone(), shared.clone());

        // Repaint on a cadence so freshly polled data is shown.
        let refresh = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        });

        Self {
            shared,
            base_url,
            view: View::Delivery,
            selected_task: None,
            selected_agent: None,
            focus: cx.focus_handle(),
            focused_once: false,
            dialog_open: false,
            dialog_requester: 0,
            dialog_caps: Vec::new(),
            dialog_cap: 0,
            dialog_request: String::new(),
            dialog_error: None,
            composer_text: String::new(),
            threads: HashMap::new(),
            caps: Arc::new(Mutex::new(HashMap::new())),
            _refresh: refresh,
        }
    }

    fn agents(&self) -> Vec<AgentSummary> {
        self.shared.lock().unwrap().agents.clone()
    }

    fn agent_by_id(&self, id: &str) -> Option<AgentSummary> {
        self.agents().into_iter().find(|a| a.id == id)
    }

    fn active_count(&self, agent_id: &str, tasks: &[FarmTask]) -> usize {
        tasks
            .iter()
            .filter(|t| t.assignee == agent_id && !t.state.terminal())
            .count()
    }

    // -- interactions ------------------------------------------------------

    fn select_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
    }

    fn open_agent_chat(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_agent = Some(id.clone());
        self.view = View::Conversations;
        self.fetch_caps(id);
        cx.notify();
    }

    fn fetch_caps(&self, id: String) {
        {
            let cache = self.caps.lock().unwrap();
            if cache.contains_key(&id) {
                return;
            }
        }
        let base = self.base_url.clone();
        let caps = self.caps.clone();
        std::thread::spawn(move || {
            if let Ok(list) = client::fetch_capabilities(&base, &id) {
                caps.lock().unwrap().insert(id, list);
            }
        });
    }

    fn boot_agent(&self, id: String) {
        let base = self.base_url.clone();
        std::thread::spawn(move || {
            let _ = client::boot_agent(&base, &id);
        });
    }

    fn stop_agent(&self, id: String) {
        let base = self.base_url.clone();
        std::thread::spawn(move || {
            let _ = client::stop_agent(&base, &id);
        });
    }

    fn boot_all(&self, snap: &Snapshot) {
        for agent in &snap.agents {
            if !snap.is_running(&agent.id) {
                self.boot_agent(agent.id.clone());
            }
        }
    }

    fn stop_all(&self, snap: &Snapshot) {
        for agent in &snap.agents {
            if snap.is_running(&agent.id) {
                self.stop_agent(agent.id.clone());
            }
        }
    }

    fn open_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog_open = true;
        self.dialog_error = None;
        self.dialog_request.clear();
        self.dialog_requester = 0;
        self.dialog_cap = 0;
        self.reload_dialog_caps();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn reload_dialog_caps(&mut self) {
        let agents = self.agents();
        self.dialog_caps.clear();
        if let Some(agent) = agents.get(self.dialog_requester) {
            if let Ok(list) = client::fetch_capabilities(&self.base_url, &agent.id) {
                self.dialog_caps = list;
            }
        }
        self.dialog_cap = 0;
    }

    fn cycle_requester(&mut self, cx: &mut Context<Self>) {
        let n = self.agents().len().max(1);
        self.dialog_requester = (self.dialog_requester + 1) % n;
        self.reload_dialog_caps();
        cx.notify();
    }

    fn cycle_capability(&mut self, cx: &mut Context<Self>) {
        if !self.dialog_caps.is_empty() {
            self.dialog_cap = (self.dialog_cap + 1) % self.dialog_caps.len();
        }
        cx.notify();
    }

    fn submit_dialog(&mut self, cx: &mut Context<Self>) {
        let agents = self.agents();
        let Some(requester) = agents.get(self.dialog_requester).cloned() else {
            self.dialog_error = Some("No requesting agent available.".into());
            cx.notify();
            return;
        };
        let Some(cap) = self.dialog_caps.get(self.dialog_cap).cloned() else {
            self.dialog_error = Some("This agent has no authorized assignments.".into());
            cx.notify();
            return;
        };
        let Some((assignee, skill)) = client::parse_capability_uri(&cap.uri) else {
            self.dialog_error = Some("Invalid capability route.".into());
            cx.notify();
            return;
        };
        let request = self.dialog_request.trim().to_string();
        if request.is_empty() {
            self.dialog_error = Some("Describe the request before assigning.".into());
            cx.notify();
            return;
        }
        match client::create_task(&self.base_url, &requester.id, &assignee, &skill, &request) {
            Ok(task) => {
                self.selected_task = Some(task.id);
                self.view = View::Delivery;
                self.dialog_open = false;
                self.dialog_request.clear();
            }
            Err(err) => {
                self.dialog_error = Some(err.to_string());
            }
        }
        cx.notify();
    }

    fn send_composer(&mut self, cx: &mut Context<Self>) {
        let Some(agent_id) = self.selected_agent.clone() else {
            return;
        };
        let text = self.composer_text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let thread = self.threads.entry(agent_id).or_default();
        thread.push(ChatMsg {
            role: "user",
            text,
        });
        thread.push(ChatMsg {
            role: "system",
            text: "Live agent replies stream from the agent's private Firecracker MicroVM. \
                   Start ironclawd with the firecracker feature and a booted guest to receive responses."
                .into(),
        });
        self.composer_text.clear();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "enter" => {
                if self.dialog_open {
                    self.submit_dialog(cx);
                } else if matches!(self.view, View::Conversations) {
                    self.send_composer(cx);
                }
                return;
            }
            "escape" => {
                if self.dialog_open {
                    self.dialog_open = false;
                    cx.notify();
                }
                return;
            }
            "backspace" => {
                if self.dialog_open {
                    self.dialog_request.pop();
                } else {
                    self.composer_text.pop();
                }
                cx.notify();
                return;
            }
            "space" => {
                if self.dialog_open {
                    self.dialog_request.push(' ');
                } else {
                    self.composer_text.push(' ');
                }
                cx.notify();
                return;
            }
            _ => {}
        }
        if let Some(ch) = event.keystroke.key_char.as_ref() {
            if !ch.is_empty() && !ch.chars().any(|c| c.is_control()) {
                if self.dialog_open {
                    self.dialog_request.push_str(ch);
                } else if matches!(self.view, View::Conversations) {
                    self.composer_text.push_str(ch);
                }
                cx.notify();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// small element helpers
// ---------------------------------------------------------------------------

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn ago(ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(ms);
    let secs = now.saturating_sub(ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

fn state_colors(state: TaskState) -> (u32, Rgba) {
    match state {
        TaskState::Completed => (ACCENT2, rgba(0x55d6be1c)),
        TaskState::Working | TaskState::Submitted | TaskState::InputRequired => {
            (WARNING, rgba(0xe9b44c1c))
        }
        TaskState::Failed | TaskState::Rejected => (DANGER, rgba(0xf26b761c)),
        TaskState::Canceled => (MUTED, rgba(0x59617133)),
    }
}

fn avatar(text: String, big: bool) -> impl IntoElement {
    let s = if big { px(50.) } else { px(36.) };
    div()
        .w(s)
        .h(s)
        .rounded(px(if big { 16. } else { 11. }))
        .bg(avatar_grad())
        .text_color(rgb(0xffffff))
        .when(big, |e| e.text_sm())
        .when(!big, |e| e.text_xs())
        .font_weight(FontWeight::BOLD)
        .flex()
        .items_center()
        .justify_center()
        .shadow(glow(0x6b5cf055, 12.))
        .child(text)
}

fn section_label(text: &str) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(rgb(0x7d8494))
        .font_weight(FontWeight::BOLD)
        .child(text.to_uppercase())
}

// ---------------------------------------------------------------------------
// render
// ---------------------------------------------------------------------------

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_once {
            window.focus(&self.focus, cx);
            self.focused_once = true;
        }
        let snap = self.shared.lock().unwrap().clone();

        let row = div()
            .flex()
            .size_full()
            .child(self.render_rail(&snap))
            .child(self.render_sidebar(&snap, cx))
            .child(self.render_main(&snap, cx))
            .child(self.render_inspector(&snap));

        let dialog = if self.dialog_open {
            Some(self.render_dialog(cx).into_any_element())
        } else {
            None
        };

        div()
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .size_full()
            .bg(grad(0x0d0f14, 0x14161d, 160.))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(row)
            .children(dialog)
    }
}

impl Workspace {
    fn render_rail(&self, snap: &Snapshot) -> impl IntoElement {
        let online = snap.connected;
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .w(px(72.))
            .h_full()
            .py(px(18.))
            .bg(rgb(RAIL))
            .border_r_1()
            .border_color(rgb(0x23262f))
            .child(
                div()
                    .w(px(46.))
                    .h(px(46.))
                    .rounded(px(15.))
                    .bg(avatar_grad())
                    .text_color(rgb(0xffffff))
                    .font_weight(FontWeight::BOLD)
                    .flex()
                    .items_center()
                    .justify_center()
                    .shadow(glow(0x6b5cf066, 16.))
                    .child("IC"),
            )
            .child(
                div()
                    .w(px(46.))
                    .h(px(46.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(ACCENT2))
                    .bg(rgb(PANEL))
                    .text_color(rgb(ACCENT2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(FontWeight::BOLD)
                    .shadow(glow(0x55d6be3d, 12.))
                    .child("E"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .w(px(11.))
                    .h(px(11.))
                    .rounded_full()
                    .bg(rgb(if online { ACCENT2 } else { 0x596171 }))
                    .when(online, |e| e.shadow(glow(0x55d6beaa, 12.))),
            )
    }

    fn render_sidebar(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let label = if snap.connected {
            "live".to_string()
        } else {
            "offline".to_string()
        };
        let channels = [
            (View::Conversations, "●", "conversations"),
            (View::Delivery, "#", "delivery"),
            (View::Team, "#", "team"),
            (View::Architecture, "#", "architecture"),
        ];
        let mut nav = div().flex().flex_col().gap(px(3.)).px(px(12.)).py(px(18.));
        for (view, prefix, name) in channels {
            let active = self.view == view;
            nav = nav.child(
                div()
                    .id(SharedElementId::channel(name))
                    .flex()
                    .gap(px(9.))
                    .items_center()
                    .w_full()
                    .px(px(11.))
                    .py(px(9.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(active, |e| {
                        e.bg(grad(0x241f3a, 0x1c2130, 90.))
                            .text_color(rgb(TEXT))
                            .border_l_2()
                            .border_color(rgb(ACCENT))
                    })
                    .when(!active, |e| {
                        e.text_color(rgb(MUTED))
                            .border_l_2()
                            .border_color(rgba(0x00000000))
                            .hover(|s| s.bg(rgb(PANEL2)).text_color(rgb(TEXT)))
                    })
                    .child(
                        div()
                            .text_color(rgb(if active { ACCENT } else { MUTED }))
                            .child(prefix),
                    )
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_view(view, cx))),
            );
        }

        let agents = snap.agents.clone();
        let mut people = div().flex().flex_col().gap(px(5.)).px(px(12.));
        for agent in &agents {
            let id = agent.id.clone();
            let selected =
                self.selected_agent.as_deref() == Some(agent.id.as_str()) && self.view == View::Conversations;
            let busy = self.active_count(&agent.id, &snap.tasks) > 0;
            let running = snap.is_running(&agent.id);
            people = people.child(
                div()
                    .id(SharedElementId::agent(&agent.id))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .p(px(8.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .border_l_2()
                    .border_color(rgba(0x00000000))
                    .when(selected, |e| {
                        e.bg(grad(0x241f3a, 0x1c2130, 90.))
                            .border_color(rgb(ACCENT))
                    })
                    .when(!selected, |e| e.hover(|s| s.bg(rgb(PANEL2))))
                    .child(avatar(initials(&agent.name), false))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(rgb(TEXT))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(agent.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .truncate()
                                    .child(agent.role.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(9.))
                            .h(px(9.))
                            .rounded_full()
                            .bg(rgb(if running {
                                ACCENT2
                            } else if busy {
                                WARNING
                            } else {
                                0x4a4f5e
                            }))
                            .when(running || busy, |e| {
                                e.shadow(glow(if running { 0x55d6beaa } else { 0xe9b44caa }, 10.))
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_agent_chat(id.clone(), cx))),
            );
        }

        div()
            .flex()
            .flex_col()
            .w(px(260.))
            .h_full()
            .bg(grad(0x181b22, 0x14161d, 180.))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .p(px(20.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(section_label("Ironclaw"))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Engineering"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .px(px(9.))
                            .py(px(5.))
                            .rounded_full()
                            .bg(rgba(if snap.connected { 0x55d6be1f } else { 0x59617133 }))
                            .border_1()
                            .border_color(rgba(if snap.connected {
                                0x55d6be55
                            } else {
                                0x59617155
                            }))
                            .child(
                                div()
                                    .w(px(6.))
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(rgb(if snap.connected { ACCENT2 } else { MUTED })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if snap.connected { ACCENT2 } else { MUTED }))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label),
                            ),
                    ),
            )
            .child(nav)
            .child(
                div()
                    .px(px(12.))
                    .pb(px(8.))
                    .child(section_label(&format!("Agents — {}", agents.len()))),
            )
            .child(people)
    }

    fn render_main(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let (prefix, title, description, chat_actions) = match self.view {
            View::Conversations => (
                "●",
                self.selected_agent
                    .as_deref()
                    .and_then(|id| self.agent_by_id(id))
                    .map(|a| a.name)
                    .unwrap_or_else(|| "conversations".into()),
                "Private agent conversations over the MicroVM channel".to_string(),
                true,
            ),
            View::Delivery => (
                "#",
                "delivery".to_string(),
                "Live A2A work across the engineering team".to_string(),
                false,
            ),
            View::Team => (
                "#",
                "team".to_string(),
                "Isolated agents with private memory and capabilities".to_string(),
                false,
            ),
            View::Architecture => (
                "#",
                "architecture".to_string(),
                "One shared control plane, private agent VMs".to_string(),
                false,
            ),
        };

        let header = div()
            .flex()
            .justify_between()
            .items_center()
            .h(px(78.))
            .px(px(24.))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .gap(px(7.))
                            .items_center()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .child(div().text_color(rgb(ACCENT)).child(prefix))
                            .child(title),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).child(description)),
            )
            .child(if chat_actions {
                div()
                    .id(SharedElementId::simple("ask"))
                    .px(px(16.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .bg(rgb(PANEL2))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(0x2c313d)))
                    .child("Ask teammate")
            } else {
                div()
                    .id(SharedElementId::simple("new-task"))
                    .px(px(17.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .bg(grad(0x9a8bff, 0x7a68f0, 160.))
                    .text_color(rgb(0xffffff))
                    .font_weight(FontWeight::BOLD)
                    .shadow(glow(0x8b7cf666, 16.))
                    .cursor_pointer()
                    .hover(|s| s.bg(grad(0xa899ff, 0x8877ff, 160.)))
                    .child("New task")
                    .on_click(cx.listener(|this, _, window, cx| this.open_dialog(window, cx)))
            });

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(self.render_metrics(snap))
            .child(self.render_content(snap, cx))
    }

    fn render_metrics(&self, snap: &Snapshot) -> impl IntoElement {
        let active = snap.tasks.iter().filter(|t| !t.state.terminal()).count();
        let completed = snap
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Completed)
            .count();
        let failed = snap
            .tasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Failed | TaskState::Rejected))
            .count();
        let cells = [
            (active.to_string(), "active", TEXT),
            (completed.to_string(), "completed", ACCENT2),
            (
                failed.to_string(),
                "needs attention",
                if failed > 0 { DANGER } else { MUTED },
            ),
            (snap.agents.len().to_string(), "agents", ACCENT),
        ];
        let mut row = div()
            .flex()
            .gap(px(1.))
            .bg(rgb(BORDER))
            .border_b_1()
            .border_color(rgb(BORDER));
        for (value, label, color) in cells {
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(4.))
                    .px(px(24.))
                    .py(px(18.))
                    .bg(grad(0x171a21, 0x121419, 180.))
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(color))
                            .child(value),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(label),
                    ),
            );
        }
        row
    }

    fn render_content(&self, snap: &Snapshot, cx: &mut Context<Self>) -> gpui::AnyElement {
        let base = div()
            .id(SharedElementId::simple("content"))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(20.));
        match self.view {
            View::Delivery => base
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(self.render_tasks(snap, cx))
                .into_any_element(),
            View::Team => base.child(self.render_team(snap, cx)).into_any_element(),
            View::Architecture => base.child(self.render_architecture(snap)).into_any_element(),
            View::Conversations => base
                .flex()
                .flex_col()
                .child(self.render_conversation(snap))
                .into_any_element(),
        }
    }

    fn render_tasks(&self, snap: &Snapshot, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut tasks = snap.tasks.clone();
        tasks.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        if tasks.is_empty() {
            return vec![empty_state("No work yet. Create the first team task.").into_any_element()];
        }
        tasks
            .into_iter()
            .map(|task| {
                let (fg, bg) = state_colors(task.state);
                let selected = self.selected_task.as_deref() == Some(task.id.as_str());
                let id = task.id.clone();
                div()
                    .id(SharedElementId::task(&task.id))
                    .flex()
                    .flex_col()
                    .p(px(17.))
                    .rounded(px(13.))
                    .bg(grad(0x1f232d, 0x1a1e27, 160.))
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .cursor_pointer()
                    .shadow(shadow_soft())
                    .when(selected, |e| e.shadow(glow(0x8b7cf655, 18.)))
                    .when(!selected, |e| {
                        e.hover(|s| s.border_color(rgb(0x4a4f5e)))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .px(px(9.))
                                    .py(px(4.))
                                    .rounded_full()
                                    .bg(bg)
                                    .child(
                                        div()
                                            .w(px(6.))
                                            .h(px(6.))
                                            .rounded_full()
                                            .bg(rgb(fg)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(fg))
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(task.state.label()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(ago(task.updated_at_ms)),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(13.))
                            .text_color(rgb(TEXT))
                            .font_weight(FontWeight::BOLD)
                            .child(task.skill.replace('_', " ")),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_color(rgb(0xc6cad4))
                            .truncate()
                            .child(task.summary()),
                    )
                    .child(
                        div()
                            .mt(px(11.))
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(task.requester.clone())
                            .child(div().text_color(rgb(ACCENT)).child("→"))
                            .child(task.assignee.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_task = Some(id.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn render_team(&self, snap: &Snapshot, cx: &mut Context<Self>) -> impl IntoElement {
        let running_count = snap.agents.iter().filter(|a| snap.is_running(&a.id)).count();
        let backend = if snap.backend.is_empty() {
            "sandbox".to_string()
        } else {
            snap.backend.clone()
        };

        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .mb(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(format!("{running_count}/{} sandboxes running", snap.agents.len())),
                    )
                    .child(
                        div()
                            .px(px(9.))
                            .py(px(4.))
                            .rounded_full()
                            .bg(rgba(0x8b7cf61f))
                            .border_1()
                            .border_color(rgba(0x8b7cf655))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(ACCENT))
                            .child(format!("backend · {backend}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .id(SharedElementId::simple("boot-all"))
                            .px(px(14.))
                            .py(px(9.))
                            .rounded(px(9.))
                            .bg(grad(0x9a8bff, 0x7a68f0, 160.))
                            .text_color(rgb(0xffffff))
                            .font_weight(FontWeight::BOLD)
                            .shadow(glow(0x8b7cf655, 14.))
                            .cursor_pointer()
                            .hover(|s| s.bg(grad(0xa899ff, 0x8877ff, 160.)))
                            .child("Boot all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let snap = this.shared.lock().unwrap().clone();
                                this.boot_all(&snap);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(SharedElementId::simple("stop-all"))
                            .px(px(14.))
                            .py(px(9.))
                            .rounded(px(9.))
                            .bg(rgb(PANEL2))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x2c313d)))
                            .child("Stop all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let snap = this.shared.lock().unwrap().clone();
                                this.stop_all(&snap);
                                cx.notify();
                            })),
                    ),
            );

        let mut grid = div().flex().flex_wrap().gap(px(12.));
        for agent in &snap.agents {
            let active = self.active_count(&agent.id, &snap.tasks);
            let running = snap.is_running(&agent.id);
            let boot_id = agent.id.clone();
            let stop_id = agent.id.clone();
            let open_id = agent.id.clone();

            let status_pill = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(9.))
                .py(px(4.))
                .rounded_full()
                .bg(rgba(if running { 0x55d6be1f } else { 0x59617126 }))
                .border_1()
                .border_color(rgba(if running { 0x55d6be55 } else { 0x59617144 }))
                .child(
                    div()
                        .w(px(6.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(rgb(if running { ACCENT2 } else { MUTED })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if running { ACCENT2 } else { MUTED }))
                        .child(if running { "running" } else { "idle" }),
                );

            let action = if running {
                div()
                    .id(SharedElementId::stop(&agent.id))
                    .flex_1()
                    .flex()
                    .justify_center()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .bg(rgba(0xf26b761c))
                    .border_1()
                    .border_color(rgba(0xf26b7655))
                    .text_color(rgb(DANGER))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xf26b7626)))
                    .child("Stop")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stop_agent(stop_id.clone());
                        cx.notify();
                    }))
            } else {
                div()
                    .id(SharedElementId::boot(&agent.id))
                    .flex_1()
                    .flex()
                    .justify_center()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .bg(grad(0x9a8bff, 0x7a68f0, 160.))
                    .text_color(rgb(0xffffff))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .shadow(glow(0x8b7cf655, 12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(grad(0xa899ff, 0x8877ff, 160.)))
                    .child("Boot")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.boot_agent(boot_id.clone());
                        cx.notify();
                    }))
            };

            grid = grid.child(
                div()
                    .id(SharedElementId::team(&agent.id))
                    .flex()
                    .flex_col()
                    .w(px(236.))
                    .p(px(20.))
                    .rounded(px(14.))
                    .bg(grad(0x1f232d, 0x1a1e27, 160.))
                    .border_1()
                    .border_color(rgb(if running { ACCENT2 } else { BORDER }))
                    .shadow(shadow_soft())
                    .hover(|s| s.border_color(rgb(ACCENT)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(avatar(initials(&agent.name), true))
                            .child(status_pill),
                    )
                    .child(
                        div()
                            .mt(px(14.))
                            .text_color(rgb(TEXT))
                            .font_weight(FontWeight::BOLD)
                            .child(agent.name.clone()),
                    )
                    .child(div().text_color(rgb(MUTED)).child(agent.role.clone()))
                    .child(
                        div()
                            .mt(px(6.))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("{} skills · {} active tasks", agent.a2a_skills, active)),
                    )
                    .child(
                        div()
                            .mt(px(16.))
                            .flex()
                            .gap(px(8.))
                            .child(action)
                            .child(
                                div()
                                    .id(SharedElementId::open(&agent.id))
                                    .px(px(12.))
                                    .py(px(8.))
                                    .rounded(px(9.))
                                    .bg(rgb(PANEL2))
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(ACCENT2))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(0x2c313d)))
                                    .child("Open →")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_agent_chat(open_id.clone(), cx)
                                    })),
                            ),
                    ),
            );
        }
        div().flex().flex_col().child(toolbar).child(grid)
    }

    fn render_architecture(&self, snap: &Snapshot) -> impl IntoElement {
        let node = |title: String, sub: Option<String>, host: bool| {
            let mut n = div()
                .flex()
                .flex_col()
                .items_center()
                .px(px(20.))
                .py(px(14.))
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(if host { ACCENT } else { BORDER }))
                .when(host, |e| {
                    e.bg(grad(0x241f3a, 0x1b2030, 150.)).shadow(glow(0x8b7cf644, 20.))
                })
                .when(!host, |e| e.bg(grad(0x1f232d, 0x1a1e27, 160.)).shadow(shadow_soft()))
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(TEXT)).child(title));
            if let Some(sub) = sub {
                n = n.child(div().mt(px(5.)).text_xs().text_color(rgb(MUTED)).child(sub));
            }
            n
        };
        let mut vms = div().flex().flex_wrap().justify_center().gap(px(8.));
        for agent in &snap.agents {
            vms = vms.child(node(
                agent.name.clone(),
                Some(format!("{} · private VM · memory · Wasm", agent.role)),
                false,
            ));
        }
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(11.))
            .py(px(20.))
            .child(node("Telegram + Workspace".into(), None, false))
            .child(div().text_xs().text_color(rgb(MUTED)).child("↓"))
            .child(node(
                "Ironclaw host".into(),
                Some("registry · task ledger · MCP broker · VM manager".into()),
                true,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("↓ authenticated A2A tasks"),
            )
            .child(vms)
    }

    fn render_conversation(&self, _snap: &Snapshot) -> impl IntoElement {
        let agent = self
            .selected_agent
            .as_deref()
            .and_then(|id| self.agent_by_id(id));
        let Some(agent) = agent else {
            return empty_state("Select an agent from the sidebar to start a private conversation.")
                .into_any_element();
        };

        let empty_thread = Vec::new();
        let messages = self.threads.get(&agent.id).unwrap_or(&empty_thread);
        let mut timeline = div().flex().flex_col().gap(px(14.)).flex_1();
        if messages.is_empty() {
            timeline = timeline.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.))
                    .py(px(40.))
                    .text_color(rgb(MUTED))
                    .child(avatar(initials(&agent.name), true))
                    .child(
                        div()
                            .text_color(rgb(TEXT))
                            .font_weight(FontWeight::BOLD)
                            .child(format!("Talk with {}", agent.name)),
                    )
                    .child(div().child(
                        "This conversation uses the agent's private MicroVM and memory.",
                    )),
            );
        }
        for msg in messages {
            let is_user = msg.role == "user";
            let is_system = msg.role == "system";
            timeline = timeline.child(
                div()
                    .max_w(px(560.))
                    .when(is_user, |e| e.ml_auto())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(if is_user {
                                "You".to_string()
                            } else if is_system {
                                "Workspace".to_string()
                            } else {
                                agent.name.clone()
                            }),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .p(px(13.))
                            .rounded(px(14.))
                            .border_1()
                            .border_color(rgb(if is_user { 0x7a68f0 } else { BORDER }))
                            .when(is_user, |e| {
                                e.bg(grad(0x6b5cf0, 0x5647c9, 150.))
                                    .text_color(rgb(0xffffff))
                                    .shadow(shadow_soft())
                            })
                            .when(is_system && !is_user, |e| {
                                e.bg(rgba(0x55d6be14))
                                    .border_color(rgba(0x55d6be44))
                                    .text_xs()
                                    .text_color(rgb(0xb8ded7))
                            })
                            .when(!is_user && !is_system, |e| e.bg(rgb(PANEL)))
                            .child(msg.text.clone()),
                    ),
            );
        }

        let composer_preview = if self.composer_text.is_empty() {
            format!("Message {}…", agent.name)
        } else {
            self.composer_text.clone()
        };
        let composer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mt(px(16.))
            .p(px(9.))
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(ACCENT))
            .bg(rgb(PANEL))
            .shadow(glow(0x8b7cf62e, 16.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.))
                    .py(px(8.))
                    .text_color(rgb(if self.composer_text.is_empty() { MUTED } else { TEXT }))
                    .child(composer_preview),
            )
            .child(
                div()
                    .px(px(17.))
                    .py(px(10.))
                    .rounded(px(10.))
                    .bg(grad(0x9a8bff, 0x7a68f0, 160.))
                    .text_color(rgb(0xffffff))
                    .font_weight(FontWeight::BOLD)
                    .shadow(glow(0x8b7cf655, 12.))
                    .child("Send"),
            );

        div()
            .flex()
            .flex_col()
            .h_full()
            .child(timeline)
            .child(composer)
            .into_any_element()
    }

    fn render_inspector(&self, snap: &Snapshot) -> impl IntoElement {
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(20.))
            .text_sm()
            .text_color(rgb(MUTED));

        let mut title = "Select a task".to_string();

        if matches!(self.view, View::Conversations | View::Team) {
            if let Some(agent) = self
                .selected_agent
                .as_deref()
                .and_then(|id| self.agent_by_id(id))
            {
                title = agent.name.clone();
                body = body
                    .child(kv("Role", agent.role.clone()))
                    .child(kv("Agent ID", agent.id.clone()))
                    .child(kv(
                        "Memory",
                        format!("{} · private VM", agent.memory_engine),
                    ))
                    .child(kv(
                        "Active work",
                        self.active_count(&agent.id, &snap.tasks).to_string(),
                    ))
                    .child(kv("Wasm tools", agent.wasm_tools.to_string()))
                    .child(kv("MCP servers", agent.mcp_servers.to_string()))
                    .child(kv("A2A skills", agent.a2a_skills.to_string()));
                let caps = self.caps.lock().unwrap().get(&agent.id).cloned();
                if let Some(caps) = caps {
                    body = body.child(inspector_heading("Authorized capabilities"));
                    for cap in caps {
                        body = body.child(
                            div()
                                .my(px(5.))
                                .px(px(9.))
                                .py(px(7.))
                                .rounded(px(8.))
                                .bg(rgba(0x55d6be14))
                                .border_1()
                                .border_color(rgba(0x55d6be3d))
                                .text_color(rgb(ACCENT2))
                                .text_xs()
                                .child(cap.uri.clone()),
                        );
                    }
                }
            }
        } else if let Some(task) = self
            .selected_task
            .as_deref()
            .and_then(|id| snap.tasks.iter().find(|t| t.id == id))
        {
            title = task.skill.replace('_', " ");
            body = body
                .child(kv("State", task.state.label().to_string()))
                .child(kv("Requester", task.requester.clone()))
                .child(kv("Assignee", task.assignee.clone()))
                .child(kv("Task", task.id.clone()))
                .child(kv("Context", task.context_id.clone()))
                .child(kv("Depth", task.delegation_depth.to_string()))
                .child(inspector_heading("Input"))
                .child(json_block(&task.input))
                .child(inspector_heading("Output"))
                .child(json_block(task.output.as_ref().unwrap_or(&serde_json::Value::Null)));
        } else {
            body = body.child(
                div().child("Task input, delegation lineage, output, and artifacts appear here."),
            );
        }

        div()
            .flex()
            .flex_col()
            .w(px(320.))
            .h_full()
            .bg(grad(0x181b22, 0x14161d, 180.))
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(20.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(section_label("Inspector"))
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child(title)),
            )
            .child(
                div()
                    .id(SharedElementId::simple("inspector-scroll"))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let agents = self.agents();
        let requester = agents
            .get(self.dialog_requester)
            .map(|a| format!("{} — {}", a.name, a.role))
            .unwrap_or_else(|| "No agents".to_string());
        let capability = self
            .dialog_caps
            .get(self.dialog_cap)
            .map(|c| {
                let route = c.uri.replace("agent://", "");
                if c.description.is_empty() {
                    route
                } else {
                    format!("{route} — {}", c.description)
                }
            })
            .unwrap_or_else(|| "No authorized assignments".to_string());
        let request_display = if self.dialog_request.is_empty() {
            "Describe the outcome, constraints, and evidence required.".to_string()
        } else {
            self.dialog_request.clone()
        };

        let field_label = |t: &str| div().text_xs().text_color(rgb(MUTED)).font_weight(FontWeight::BOLD).child(t.to_string());

        let panel = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .w(px(560.))
            .p(px(24.))
            .rounded(px(16.))
            .bg(grad(0x1c2029, 0x161922, 160.))
            .border_1()
            .border_color(rgb(0x363c49))
            .shadow(shadow_deep())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(section_label("A2A assignment"))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("Create engineering task"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(field_label("Requesting agent (click to change)"))
                    .child(
                        div()
                            .id(SharedElementId::simple("dlg-requester"))
                            .px(px(12.))
                            .py(px(11.))
                            .rounded(px(9.))
                            .bg(rgb(0x0c0e12))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(0x4a4f5e)))
                            .child(requester)
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_requester(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(field_label("Assignment (click to change)"))
                    .child(
                        div()
                            .id(SharedElementId::simple("dlg-cap"))
                            .px(px(12.))
                            .py(px(11.))
                            .rounded(px(9.))
                            .bg(rgb(0x0c0e12))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .cursor_pointer()
                            .hover(|s| s.border_color(rgb(0x4a4f5e)))
                            .child(capability)
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_capability(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(field_label("Request (type here)"))
                    .child(
                        div()
                            .min_h(px(96.))
                            .px(px(12.))
                            .py(px(11.))
                            .rounded(px(9.))
                            .bg(rgb(0x0c0e12))
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .shadow(glow(0x8b7cf62e, 14.))
                            .text_color(rgb(if self.dialog_request.is_empty() { MUTED } else { TEXT }))
                            .child(request_display),
                    ),
            )
            .children(self.dialog_error.clone().map(|e| {
                div().text_xs().text_color(rgb(DANGER)).child(e)
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(9.))
                    .child(
                        div()
                            .id(SharedElementId::simple("dlg-cancel"))
                            .px(px(16.))
                            .py(px(10.))
                            .rounded(px(9.))
                            .bg(rgb(PANEL2))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(0x2c313d)))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id(SharedElementId::simple("dlg-submit"))
                            .px(px(18.))
                            .py(px(10.))
                            .rounded(px(9.))
                            .bg(grad(0x9a8bff, 0x7a68f0, 160.))
                            .text_color(rgb(0xffffff))
                            .font_weight(FontWeight::BOLD)
                            .shadow(glow(0x8b7cf666, 16.))
                            .cursor_pointer()
                            .hover(|s| s.bg(grad(0xa899ff, 0x8877ff, 160.)))
                            .child("Assign task")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_dialog(cx))),
                    ),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x05070ad9))
            .child(panel)
    }
}

// ---------------------------------------------------------------------------
// stand-alone element builders
// ---------------------------------------------------------------------------

fn empty_state(text: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedElementId::simple("empty"))
        .flex()
        .items_center()
        .justify_center()
        .p(px(40.))
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(BORDER))
        .text_color(rgb(MUTED))
        .child(text.to_string())
}

fn kv(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .justify_between()
        .gap(px(12.))
        .py(px(9.))
        .border_b_1()
        .border_color(rgb(BORDER))
        .child(div().text_color(rgb(MUTED)).child(label.to_string()))
        .child(
            div()
                .text_color(rgb(TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn inspector_heading(text: &str) -> impl IntoElement {
    div()
        .mt(px(22.))
        .mb(px(9.))
        .text_xs()
        .text_color(rgb(ACCENT2))
        .font_weight(FontWeight::BOLD)
        .child(text.to_uppercase())
}

fn json_block(value: &serde_json::Value) -> impl IntoElement {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    div()
        .p(px(12.))
        .rounded(px(10.))
        .bg(rgb(0x0c0e12))
        .border_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .text_color(rgb(0xcbd2df))
        .child(text)
}

/// Helper for building stable element ids.
struct SharedElementId;
impl SharedElementId {
    fn simple(name: &'static str) -> gpui::ElementId {
        gpui::ElementId::Name(name.into())
    }
    fn channel(name: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("channel-{name}").into())
    }
    fn agent(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("agent-{id}").into())
    }
    fn team(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("team-{id}").into())
    }
    fn boot(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("boot-{id}").into())
    }
    fn stop(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("stop-{id}").into())
    }
    fn open(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("open-{id}").into())
    }
    fn task(id: &str) -> gpui::ElementId {
        gpui::ElementId::Name(format!("task-{id}").into())
    }
}

fn main() {
    let base_url =
        std::env::var("IRONCLAW_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:9938".to_string());

    application().run(move |cx: &mut App| {
        let bounds = Bounds {
            origin: point(px(8.), px(30.)),
            size: size(px(1900.), px(1140.)),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| Workspace::new(base_url.clone(), cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
