# Five-person engineering team demo

This demo runs one owner-only Telegram bot in front of five specialized agents:

```text
Maya — Product Manager
└── Ravi — Engineering Lead
    ├── Nora — Backend Engineer
    ├── Leo — Frontend Engineer
    └── Zoe — QA Engineer
```

Each selected agent uses its own VM, transcript, workspace, and memory. Assignments are durable A2A
tasks. The engineering lead can delegate implementation and verification to the three specialists.

## Start it

Create `.env` and set:

```bash
OPENAI_API_KEY=...
TELEGRAM_BOT_TOKEN=...
OWNER_TELEGRAM_CHAT_ID=...
```

The token comes from Telegram's BotFather. Send the bot `/start`, call Telegram `getUpdates`, and
use the numeric `message.chat.id` as `OWNER_TELEGRAM_CHAT_ID`. Only that chat is accepted.

Build the VM image if necessary, then run the demo:

```bash
./scripts/build-ubuntu-rootfs.sh
./scripts/run-engineering-team-demo.sh
```

Open <http://127.0.0.1:9938/ui> for the team workspace.

## Verified workspace screenshots

The workspace has been exercised in Chromium against a live local daemon, including loading all
five agents, selecting a valid A2A capability, submitting a task, and observing its completed state.

![Five-agent team view](../../docs/screenshots/engineering-workspace-team.png)

![Capability-aware task assignment](../../docs/screenshots/engineering-workspace-new-task.png)

![Completed task and inspector](../../docs/screenshots/engineering-workspace-completed-task.png)

## Telegram walkthrough

```text
/team
/agent product-manager
Write acceptance criteria for usage-based billing.
/assign engineering-lead lead_delivery Build usage-based billing. Delegate the API,
frontend, and release verification; return tested evidence.
/tasks
```

After the delivery task completes:

```text
/agent engineering-lead
/tasks
Summarize the implementation decisions and remaining risks.
```

Other useful assignments include:

```text
/assign backend-engineer implement_backend Implement the metering API and tests.
/assign frontend-engineer implement_frontend Implement the usage dashboard and accessibility tests.
/assign qa-engineer verify_release Verify the billing flow and report release risk.
```

Assignments are authorized from the currently selected agent. For example, the product manager can
assign the engineering lead but cannot bypass the lead and directly assign an engineer.

## What the workspace currently provides

- live agent roster and per-agent active-task count;
- delivery board backed by the durable farm task ledger;
- capability-aware A2A task creation;
- task input, output, context, delegation depth, and artifact inspection;
- an architecture view showing the shared host and private agent VMs.

The workspace is deliberately the first operational slice, not yet a complete Slack clone. The
next layer is persistent channels and threads where humans and agents can mention one another while
task events appear in the same conversation timeline.
