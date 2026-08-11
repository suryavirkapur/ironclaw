export const REPOSITORY_NAMES = [
  'payments-api',
  'booking-web',
  'notifications-worker',
  'identity-service',
  'analytics-pipeline',
  'partner-gateway',
] as const;

export interface RepositorySnapshot {
  name: string;
  files: Record<string, string>;
}

export interface RepositoryStore {
  list(): string[];
  read(name: string): RepositorySnapshot;
}

const snapshots: RepositorySnapshot[] = [
  repository('payments-api', {
    node: '18',
    actions: ['actions/checkout@v4', 'actions/setup-node@v4'],
    dockerUser: null,
    healthcheck: false,
    dependencies: { express: '4.18.3', lodash: '4.17.20' },
    env: { DEBUG: 'true', API_KEY: 'pk_live_demo_value' },
  }),
  repository('booking-web', {
    node: '20',
    actions: ['actions/checkout@v4', 'actions/setup-node@v4'],
    dockerUser: 'node',
    healthcheck: true,
    dependencies: { express: '4.19.2', lodash: '4.17.21' },
    env: { DEBUG: 'false' },
  }),
  repository('notifications-worker', {
    node: '16',
    actions: ['actions/checkout@main', 'docker/build-push-action@v6'],
    dockerUser: 'node',
    healthcheck: false,
    dependencies: { express: '4.19.2', lodash: '4.17.21' },
    env: { DEBUG: 'false' },
  }),
  repository('identity-service', {
    node: '20',
    actions: ['actions/checkout@v4', 'actions/setup-node@master'],
    dockerUser: null,
    healthcheck: true,
    dependencies: { express: '4.17.1', lodash: '4.17.21' },
    env: { DEBUG: 'false', JWT_SECRET: 'hardcoded-demo-secret' },
  }),
  repository('analytics-pipeline', {
    node: '18',
    actions: ['actions/checkout@v3', 'actions/setup-node@v4'],
    dockerUser: 'node',
    healthcheck: true,
    dependencies: { express: '4.19.2', lodash: '4.17.19' },
    env: { DEBUG: 'true' },
  }),
  repository('partner-gateway', {
    node: '20',
    actions: ['actions/checkout@v4', 'actions/setup-node@v4'],
    dockerUser: '10001',
    healthcheck: false,
    dependencies: { express: '4.19.2', lodash: '4.17.21' },
    env: { DEBUG: 'false', PARTNER_TOKEN: 'token-demo-123' },
  }),
];

export function createRepositoryStore(): RepositoryStore {
  const byName = new Map(snapshots.map((snapshot) => [snapshot.name, snapshot]));
  return {
    list: () => [...byName.keys()].sort(),
    read: (name) => {
      const snapshot = byName.get(name);
      if (!snapshot) {
        throw new Error(`No repository "${name}". Available: ${[...byName.keys()].sort().join(', ')}`);
      }
      return structuredClone(snapshot);
    },
  };
}

interface RepositoryOptions {
  node: string;
  actions: string[];
  dockerUser: string | null;
  healthcheck: boolean;
  dependencies: Record<string, string>;
  env: Record<string, string>;
}

function repository(name: string, options: RepositoryOptions): RepositorySnapshot {
  const dockerLines = [
    `FROM node:${options.node}-alpine`,
    'WORKDIR /app',
    'COPY . .',
    ...(options.dockerUser ? [`USER ${options.dockerUser}`] : []),
    ...(options.healthcheck ? ['HEALTHCHECK CMD wget -qO- http://localhost:3000/health || exit 1'] : []),
    'CMD ["node", "server.js"]',
  ];
  const workflowUses = options.actions.map((action) => `      - uses: ${action}`).join('\n');
  const env = Object.entries(options.env)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n');

  return {
    name,
    files: {
      'package.json': JSON.stringify(
        {
          name,
          engines: { node: `>=${options.node}` },
          dependencies: options.dependencies,
        },
        null,
        2,
      ),
      Dockerfile: dockerLines.join('\n'),
      '.github/workflows/ci.yml': `name: ci\njobs:\n  test:\n    steps:\n${workflowUses}\n`,
      '.env.production': `${env}\n`,
    },
  };
}
