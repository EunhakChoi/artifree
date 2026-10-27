import type { Component, Layer, ServiceDescription } from '@artifree/contracts';

declare const INVENTORY: ServiceDescription;
const inventory = INVENTORY;
const repo = 'https://github.com/EunhakChoi/artifree';
const originalDocument = `<!doctype html>\n${document.documentElement.outerHTML}`;

function element(id: string): HTMLElement {
  const result = document.getElementById(id);
  if (!(result instanceof HTMLElement))
    throw new Error(`Missing document element: ${id}`);
  return result;
}

function setText(id: string, value: string): void {
  element(id).textContent = value;
}

interface ModuleNote {
  purpose: string;
  exists: string;
  next: string;
}

const notes: Record<string, ModuleNote> = {
  web: {
    purpose: '사용자가 결과물을 찾고, 편집하고, 실행하는 작업 화면입니다.',
    exists:
      'access token 연결, 아티팩트 목록·생성, 텍스트 파일 편집과 조건부 저장·충돌 표시, revision별 sandbox 미리보기. 변경 피드를 따라 목록·열린 아티팩트·최신 미리보기를 갱신하고, 고정 미리보기와 미저장 초안은 유지합니다. 페이지의 읽기는 한 scheduler가 한 번에 하나씩, 보이는 동안만, Retry-After 뒤에 시작합니다. build-time module inventory.',
    next: '파일 추가·삭제와 revision 비교, 게시 상태 표시를 같은 HTTP client로 연결합니다.',
  },
  cli: {
    purpose: '터미널과 외부 agent가 사용하는 작은 TypeScript client입니다.',
    exists:
      'artifact create·list·show·history·save·cat·preview. stdout JSON, 재시도용 idempotency key, 충돌·인증·사용법 오류의 종료 코드 구분.',
    next: 'publish·export 명령을 같은 HTTP client에 추가합니다. 별도 저장 규칙을 만들지 않습니다.',
  },
  runtime: {
    purpose: '아티팩트 내부 코드에 허용된 기능을 제공하는 브라우저 SDK입니다.',
    exists:
      'capability 이름과 세션 binding 타입만 있습니다. 실행·검증 코드는 없습니다.',
    next: 'origin·송신 창·세션을 검증하는 통신과 서버의 최종 권한 검사부터 구현합니다.',
  },
  api: {
    purpose:
      'HTTP와 MCP 요청을 동일한 application command로 전달하는 진입점입니다.',
    exists:
      'Axum/Tokio HTTP API v1(bearer token, Idempotency-Key, JSON 오류), 같은 token과 공통 operations를 쓰는 MCP Streamable HTTP 도구 8개(/mcp, 공식 rmcp SDK, stateless), HTTP·MCP 합산 사용자별 요청 빈도·동시 실행 제한(429 rate_limited), 커밋 변경 피드(GET /v1/changes SSE: cursor 재개·reset·사용자별 stream 수 제한), 별도 origin의 미리보기 서버. ticket은 짧게 유지되고 요청마다 권한을 다시 확인합니다.',
    next: '다중 사용자 인증·OAuth와 원격 공개용 Host 정책을 결정합니다. handoff는 별도 기능입니다.',
  },
  domain: {
    purpose:
      '제품의 핵심 개념을 외부 도구에 의존하지 않는 타입으로 표현합니다.',
    exists:
      '검증된 ID·title·request key, 이식 가능한 package path(탈출·대소문자 충돌·예약 이름 거절), 크기·개수 제한, 별도 version counter, DB 식별자를 포함한 변경 피드 cursor.',
    next: '구조화된 문서의 node·operation 타입을 파일 패키지와 별도로 정의합니다.',
  },
  artifacts: {
    purpose: '아티팩트의 원본 파일과 불변 revision, 게시 대상을 관리합니다.',
    exists:
      '생성, 기준 revision 조건부 commit, 같은 key 재시도 replay와 다른 요청 거절, revision 고정 읽기.',
    next: '게시 대상 revision 고정, 복원·복제를 앱 데이터·권한과 분리해 구현합니다.',
  },
  policy: {
    purpose: '사용자·agent·runtime이 수행할 수 있는 작업을 판정합니다.',
    exists:
      '소유자만 접근하는 판정을 모든 읽기·쓰기와 미리보기 요청마다 적용합니다. 접근할 수 없는 대상은 존재 여부를 숨깁니다.',
    next: '공유·역할·권한 철회와 runtime capability를 정의하고 철회 후 거절을 테스트합니다.',
  },
  documents: {
    purpose: '구조화된 문서 편집과 실행 중 앱 데이터를 각각 관리합니다.',
    exists:
      '저장 모델 분리 원칙을 기록한 모듈. 편집기와 DB는 연결되지 않았습니다.',
    next: '문서 sequence와 앱 data version을 구분하고 타입별 편집 연산을 정의합니다.',
  },
  jobs: {
    purpose: '변경 알림, 긴 작업, 재시도와 연결 복구를 조정합니다.',
    exists: 'outbox·작업 수명·취소 의미를 기록한 모듈. 큐나 실행기는 없습니다.',
    next: '커밋 이벤트와 worker 수명, 중복 실행 방지, 실패·재시작·취소를 함께 설계합니다.',
  },
  handoff: {
    purpose:
      '사용자가 선택한 영역과 수정 의도를 외부 agent session으로 전달합니다.',
    exists:
      '모듈 책임 정의. 내장 agent 실행기나 모델 계정은 필요하지 않습니다.',
    next: 'agent별 연결 adapter보다 먼저 선택 영역·기준 revision·요청의 공통 형식을 정의합니다.',
  },
  storage: {
    purpose: '메타데이터와 파일 bytes를 환경에 맞게 보관하는 adapter입니다.',
    exists:
      '로컬 SQLite 한 파일: WAL·synchronous=FULL, BEGIN IMMEDIATE 조건부 commit, 같은 transaction의 변경 피드 기록, 동일 내용 공유 저장, schema version 확인과 v1→v2 migration.',
    next: 'PostgreSQL·object storage adapter에 같은 계약 테스트를 적용합니다. blob과 SQL commit 경계를 명시합니다.',
  },
  models: {
    purpose:
      '아티팩트가 AI 기능을 사용할 때 선택적으로 호출하는 provider입니다.',
    exists:
      '모듈 책임과 credential 경계 정의. 모델 SDK나 키를 요구하지 않습니다.',
    next: '선정한 공급자의 streaming·사용량·한도·최선의 취소를 공통 결과 형식으로 연결합니다.',
  },
  tools: {
    purpose: '외부 MCP 도구와 명시적으로 허용된 프로젝트 파일을 연결합니다.',
    exists: 'outbound provider 모듈. inbound MCP server와 역할을 구분했습니다.',
    next: '사용자 연결별 tool 권한과 파일 경계를 확인하고, 요청·취소·결과를 연결합니다.',
  },
  build: {
    purpose: '원본 파일 패키지를 실행 가능한 산출물로 변환합니다.',
    exists:
      '격리 worker의 책임 정의. 제품용 bundler·격리 환경은 구성하지 않았습니다.',
    next: '지원할 입력과 의존성 정책을 먼저 정하고, 작업별 자원·네트워크 제한을 적용합니다.',
  },
  export: {
    purpose: '문서·이미지·슬라이드·영상 등의 다운로드 결과를 만듭니다.',
    exists: '변환 provider 위치와 후보 범위만 정의했습니다.',
    next: 'Playwright·LibreOffice·FFmpeg 등을 샘플 품질과 배포 비용으로 비교하고 worker 언어를 선택합니다.',
  },
  contracts: {
    purpose: 'Rust와 TypeScript가 같은 전송 데이터 형식을 사용하도록 합니다.',
    exists:
      'descriptor와 HTTP API v1 요청·응답 DTO → JSON Schema → TypeScript 생성, MCP 도구 catalog(인자·결과 schema) 생성, 잘못된 입력·drift 검증.',
    next: 'runtime 메시지를 실제 기능 구현에 맞춰 추가합니다. 서버 입력 검증은 계약과 별도로 유지합니다.',
  },
  client: {
    purpose: 'Web과 CLI가 같은 HTTP 계약을 사용하는 client package입니다.',
    exists:
      'fetch 기반 HTTP API v1 client. 응답을 runtime에 검증하고 오류를 code·HTTP status·current revision으로 구분합니다. 변경 피드를 WHATWG 규칙으로 해석하고, 연결 실패·429·5xx에서만 마지막 cursor부터 재연결하며 AbortSignal로 즉시 중단합니다.',
    next: '일반 요청의 취소와 재시도 정책을 web·CLI가 공유하도록 추가합니다.',
  },
};

const layerConfig: {
  layer: Layer;
  number: string;
  title: string;
  location: string;
}[] = [
  {
    layer: 'presentation',
    number: '01',
    title: 'Presentation layer',
    location: 'TypeScript · client side',
  },
  {
    layer: 'logic',
    number: '02',
    title: 'Logic layer',
    location: 'Rust · server side',
  },
  {
    layer: 'provider',
    number: '03',
    title: 'Provider layer',
    location: 'Adapters + isolated workers',
  },
];

const statusLabels: Record<Component['status'], string> = {
  implemented: 'Working',
  scaffold: 'Scaffold exists',
  planned: 'Planned',
};

const statusTags: Record<Component['status'], string> = {
  implemented: 'WORKING · 범위와 한계는 아래 확인',
  scaffold: 'SCAFFOLD EXISTS · 기능 범위는 아래 확인',
  planned: 'PLANNED · 동작 구현 전',
};

function sourceUrl(component: Component): string {
  const kind = component.path.endsWith('.rs') ? 'blob' : 'tree';
  return `${repo}/${kind}/main/${component.path}`;
}

function nodeFor(component: Component): HTMLButtonElement {
  const button = document.createElement('button');
  button.className = 'module-node';
  button.dataset['module'] = component.id;
  button.dataset['status'] = component.status;
  button.setAttribute('aria-pressed', 'false');
  const title = document.createElement('strong');
  title.textContent = component.name;
  const state = document.createElement('small');
  const dot = document.createElement('span');
  dot.className = 'module-dot';
  state.append(dot, statusLabels[component.status]);
  button.append(title, state);
  button.addEventListener('click', () => selectModule(component.id));
  return button;
}

function renderMap(): void {
  const map = element('layer-map');
  for (const config of layerConfig) {
    const block = document.createElement('div');
    block.className = `layer-block ${config.layer}`;
    const heading = document.createElement('div');
    heading.className = 'layer-heading';
    const number = document.createElement('span');
    number.textContent = config.number;
    const title = document.createElement('strong');
    title.textContent = config.title;
    const location = document.createElement('small');
    location.textContent = config.location;
    heading.append(number, title, location);
    const grid = document.createElement('div');
    grid.className = 'module-grid';
    for (const component of inventory.components.filter(
      (item) => item.layer === config.layer,
    ))
      grid.append(nodeFor(component));
    block.append(heading, grid);
    map.append(block);
  }
  for (const component of inventory.components.filter(
    (item) => item.layer === 'shared',
  ))
    element('shared-modules').append(nodeFor(component));
}

function selectModule(id: string): void {
  const index = inventory.components.findIndex(
    (component) => component.id === id,
  );
  const component = inventory.components[index];
  const note = notes[id];
  if (component === undefined || note === undefined)
    throw new Error(`Unknown module: ${id}`);
  document
    .querySelectorAll<HTMLButtonElement>('[data-module]')
    .forEach((button) =>
      button.setAttribute(
        'aria-pressed',
        String(button.dataset['module'] === id),
      ),
    );
  setText(
    'inspector-index',
    `${String(index + 1).padStart(2, '0')} / ${inventory.components.length}`,
  );
  const container = element('inspector-content');
  container.replaceChildren();
  const symbol = document.createElement('div');
  symbol.className = 'inspector-symbol';
  symbol.textContent = String(index + 1).padStart(2, '0');
  const heading = document.createElement('h3');
  heading.textContent = component.name;
  const description = document.createElement('p');
  description.className = 'inspector-description';
  description.textContent = note.purpose;
  const status = document.createElement('span');
  status.className = 'tag';
  status.textContent = statusTags[component.status];
  container.append(symbol, heading, description, status);
  const addField = (
    label: string,
    text: string,
    className = 'inspector-text',
  ): void => {
    const labelNode = document.createElement('p');
    labelNode.className = 'inspector-label';
    labelNode.textContent = label;
    const content = document.createElement('p');
    content.className = className;
    content.textContent = text;
    container.append(labelNode, content);
  };
  addField('LANGUAGE / BOUNDARY', component.language);
  addField('CURRENT STATE', note.exists);
  const sourceLabel = document.createElement('p');
  sourceLabel.className = 'inspector-label';
  sourceLabel.textContent = 'ACTUAL SOURCE PATH';
  const link = document.createElement('a');
  link.className = 'source-path';
  link.href = sourceUrl(component);
  link.target = '_blank';
  link.rel = 'noopener noreferrer';
  link.textContent = `${component.path} ↗`;
  container.append(sourceLabel, link);
  addField('NEXT IMPLEMENTATION', note.next, 'inspector-next');
}

function renderTree(): void {
  const paths = [
    'apps/web',
    'apps/cli',
    'packages/contracts',
    'packages/client',
    'packages/runtime-sdk',
    'crates/artifree-domain',
    'crates/artifree-core',
    'crates/artifree-ports',
    'crates/artifree-providers',
    'crates/artifree-contracts',
    'crates/artifree-server',
    'contracts/generated',
    'docs/architecture',
    'scripts',
    '.github/workflows',
  ];
  for (const path of paths) {
    const link = document.createElement('a');
    link.className = 'tree-link';
    link.href = `${repo}/tree/main/${path}`;
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
    const code = document.createElement('code');
    code.textContent = path;
    const arrow = document.createElement('span');
    arrow.textContent = '↗';
    link.append(code, arrow);
    element('source-tree').append(link);
  }
}

type Mode = 'local' | 'server' | 'hosted';
type AccessRoute = 'direct' | 'tunnel' | 'private';
let mode: Mode = 'local';
let persistent = true;
let accessRoute: AccessRoute = 'direct';

function renderDeployment(): void {
  const title: Record<Mode, string> = {
    local: '내 PC에서, 가볍게 시작합니다.',
    server: '내 서버에서, 계속 사용합니다.',
    hosted: 'Web과 Rust API를 나누어 배포합니다.',
  };
  const descriptions: Record<Mode, string> = {
    local:
      '동일한 Rust core에 로컬용 저장 adapter를 연결합니다. 세션 단위 사용과 영속 보관을 선택할 수 있게 설계합니다.',
    server:
      'Web·API·content gateway와 worker를 운영 환경에 맞게 배치합니다. 처음에는 하나의 application core를 유지합니다.',
    hosted:
      'Vercel 등은 정적 Web의 배포 위치로 사용합니다. Rust API와 긴 변환 작업은 별도 서버·컨테이너에 두는 것을 기본안으로 잡습니다.',
  };
  setText(
    'profile-label',
    `${mode.toUpperCase()} / ${persistent ? 'PERSISTENT' : 'TEMPORARY'}`,
  );
  setText('profile-title', title[mode]);
  setText('profile-description', descriptions[mode]);
  setText(
    'profile-web',
    mode === 'hosted'
      ? 'Vercel / static hosting'
      : mode === 'local'
        ? '로컬 Web assets · 별도 콘텐츠 origin 계획'
        : '서버의 정적 Web · content gateway 분리',
  );
  setText(
    'profile-api',
    mode === 'local'
      ? '로컬 Rust process · transport 미정'
      : mode === 'hosted'
        ? '외부 Rust API · 인증 / CORS 정책 필요'
        : '상주 Rust service · framework / runtime 미정',
  );
  const storage =
    mode === 'hosted'
      ? persistent
        ? '외부 PostgreSQL + object storage 후보'
        : '외부 TTL session store + TTL objects'
      : persistent
        ? mode === 'local'
          ? 'SQLite + local files 후보'
          : 'PostgreSQL + files / S3 후보'
        : 'Memory + temporary files · 동일 인스턴스 범위';
  setText('profile-storage', storage);
  setText(
    'profile-workers',
    mode === 'local'
      ? '필요한 기능의 로컬 격리 worker'
      : '별도 격리 worker · 긴 작업과 변환 처리',
  );
  const routes: Record<AccessRoute, string> = {
    direct:
      mode === 'local'
        ? 'localhost / IP · bind와 인증 설정은 후속 구현'
        : '도메인 / IP · reverse proxy와 TLS 구성',
    tunnel: 'ngrok / Cloudflare Tunnel · UI / API / content 경로 구성',
    private: 'Headscale 기반 사설 네트워크 · 내부 주소로 접근',
  };
  setText('profile-access', routes[accessRoute]);
  setText(
    'profile-rule',
    mode === 'hosted'
      ? '임시 사용도 여러 요청이 공유하는 외부 상태가 필요합니다. 함수 메모리를 영속 저장소로 사용하지 않습니다.'
      : persistent
        ? '보관 위치, 백업·복구와 provider별 동일 동작 검증을 함께 정합니다. 앱 데이터는 source revision과 따로 관리합니다.'
        : '임시의 의미는 세션 수명·TTL·정리 정책으로 정의합니다. 메모리와 파일의 종료 시 정리를 명시적으로 구현해야 합니다.',
  );
}

document
  .querySelectorAll<HTMLButtonElement>('[data-mode]')
  .forEach((button) => {
    button.addEventListener('click', () => {
      const value = button.dataset['mode'];
      if (value !== 'local' && value !== 'server' && value !== 'hosted')
        throw new Error('Invalid deployment mode.');
      mode = value;
      document
        .querySelectorAll<HTMLButtonElement>('[data-mode]')
        .forEach((candidate) =>
          candidate.setAttribute('aria-pressed', String(candidate === button)),
        );
      renderDeployment();
    });
  });

const persistence = element('persistent');
if (!(persistence instanceof HTMLInputElement))
  throw new Error('Expected persistence checkbox.');
persistence.addEventListener('change', () => {
  persistent = persistence.checked;
  renderDeployment();
});
const accessSelect = element('access');
if (!(accessSelect instanceof HTMLSelectElement))
  throw new Error('Expected access selector.');
accessSelect.addEventListener('change', () => {
  const value = accessSelect.value;
  if (value !== 'direct' && value !== 'tunnel' && value !== 'private')
    throw new Error('Invalid access route.');
  accessRoute = value;
  renderDeployment();
});

const flowSteps = [
  {
    tab: '파일 전달',
    layer: 'WORKING · PRESENTATION → LOGIC',
    title: 'agent가 원본 파일을 전달합니다.',
    copy: 'agent가 선택한 도구로 HTML·CSS·이미지 등을 생성합니다. HTTP·MCP·CLI는 같은 명령과 결과 형식을 사용합니다.',
    rule: '아티팩트 생성에 특정 모델 계정이나 내장 agent 실행기를 요구하지 않습니다.',
    code: 'PublishIntent\n  actor + artifact\n  base revision + file changes\n  idempotency key\n\n→ common application command',
  },
  {
    tab: '검증·권한',
    layer: 'WORKING · LOGIC / CORE',
    title: '권한과 변경 조건을 확인합니다.',
    copy: '서버에서 actor의 현재 권한과 파일 경로·크기·입력 형식을 확인합니다. 수정 요청은 기준 revision과 현재 상태를 비교합니다.',
    rule: '컴파일된 DTO나 iframe이 보내온 사용자 ID만으로 권한을 부여하지 않습니다.',
    code: 'authorize(actor, operation)\nvalidate(package, limits)\ncheck(base_revision, current_head)\n\n→ explicit rejection or proceed',
  },
  {
    tab: '원본 저장',
    layer: 'WORKING (LOCAL SQLITE) · LOGIC → STORAGE',
    title: 'revision과 성공 결과를 기록합니다.',
    copy: '새 원본 revision, 중복 방지 receipt와 이벤트를 일관되게 기록합니다. Blob 저장과 SQL 트랜잭션의 경계도 명시합니다.',
    rule: '화면의 pending 표시와 저장 성공을 구분합니다. 재시도로 revision이 중복 생성되지 않게 합니다.',
    code: 'SQLite transaction (local profile):\n  replay or reject request key\n  conditional revision commit\n  files + receipt\n\n→ committed revision / conflict\n(event outbox: planned)',
  },
  {
    tab: '격리 빌드',
    layer: 'PLANNED · LOGIC → BUILD PROVIDER',
    title: '빌드는 별도 작업자가 수행합니다.',
    copy: '커밋한 원본 revision을 고정해 빌드합니다. Worker의 입력·네트워크·시간·메모리와 결과 형식을 제한합니다.',
    rule: '빌드 실패가 기존 공개 버전을 덮어쓰지 않게 합니다. 번들러 자체를 sandbox로 취급하지 않습니다.',
    code: 'BuildJob\n  pinned source revision\n  toolchain + dependency policy\n  resource / network limits\n\n→ output manifest or build error',
  },
  {
    tab: '실행·보기',
    layer: 'WORKING (STATIC HTML) · PRESENTATION / RUNTIME',
    title: '고정된 버전을 분리해 보여줍니다.',
    copy: '본체 계정 UI와 분리한 콘텐츠 환경에서 특정 revision의 산출물을 제공합니다. 게시 대상 변경과 초안 편집은 별도 작업입니다.',
    rule: '실행 환경의 origin·cookie·sandbox 정책을 함께 설계합니다. 계정 자격과 모델 키를 콘텐츠에 주지 않습니다.',
    code: 'Workspace origin\n  ↓ scoped runtime session\nSeparate content origin\n  → pinned output files\n  → client-side rendering',
  },
  {
    tab: '기능 요청',
    layer: 'PLANNED · RUNTIME → CORE → PROVIDER',
    title: '허용된 기능만 서버를 통해 요청합니다.',
    copy: '앱 데이터·모델·파일·외부 도구는 세션에 고정된 capability를 사용합니다. 서버에서 현재 권한과 한도를 다시 검사합니다.',
    rule: '세션 교체·로그아웃·권한 철회 후 이전 메시지를 적용하지 않습니다. 외부 작업의 취소 의미를 구분합니다.',
    code: 'Runtime SDK request\n  → verified broker / session\n  → current authorization\n  → provider operation\n\n→ scoped result / explicit error',
  },
];

function selectFlow(index: number): void {
  const step = flowSteps[index];
  if (step === undefined) throw new Error('Unknown workflow step.');
  document
    .querySelectorAll<HTMLButtonElement>('[data-step]')
    .forEach((button) =>
      button.setAttribute(
        'aria-pressed',
        String(Number(button.dataset['step']) === index),
      ),
    );
  setText('flow-layer', step.layer);
  setText('flow-title', step.title);
  setText('flow-copy', step.copy);
  setText('flow-rule', step.rule);
  setText('flow-code', step.code);
}

function renderFlow(): void {
  flowSteps.forEach((step, index) => {
    const button = document.createElement('button');
    button.dataset['step'] = String(index);
    button.setAttribute('aria-pressed', 'false');
    const number = document.createElement('span');
    number.textContent = String(index + 1).padStart(2, '0');
    const label = document.createElement('strong');
    label.textContent = step.tab;
    button.append(number, label);
    button.addEventListener('click', () => selectFlow(index));
    element('flow-tabs').append(button);
  });
  selectFlow(0);
}

element('print').addEventListener('click', () => window.print());
element('download-html').addEventListener('click', () => {
  const blob = new Blob([originalDocument], {
    type: 'text/html;charset=utf-8',
  });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = 'artifree-architecture.html';
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
});

const observer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        document
          .querySelectorAll<HTMLAnchorElement>('.sidebar nav a')
          .forEach((link) => {
            if (link.hash === `#${entry.target.id}`)
              link.setAttribute('aria-current', 'location');
            else link.removeAttribute('aria-current');
          });
      }
    }
  },
  { rootMargin: '-10% 0px -65% 0px', threshold: 0 },
);
document
  .querySelectorAll('main > section[id]')
  .forEach((section) => observer.observe(section));

renderMap();
renderTree();
selectModule('web');
renderDeployment();
renderFlow();
