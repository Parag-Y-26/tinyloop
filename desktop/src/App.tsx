import { useEffect, useState } from 'react';
import { AnimatePresence, motion } from 'motion/react';

type Status = 'done' | 'running' | 'blocked' | 'pending';
type Step = { id: number; title: string; detail: string; status: Status; meta: string };
type Mode = 'ask' | 'auto' | 'dry';
type UiEvent = { kind: string; title: string; detail: string; status: Status; meta: string };
const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

export default function App() {
  const [steps, setSteps] = useState<Step[]>([
    { id: 1, title: 'Plan', detail: 'submit_plan(3 steps) · Rust validated', status: 'done', meta: '0.4s' },
    { id: 2, title: 'Act', detail: 'read_file src/tax.rs offset=0 limit=120', status: 'done', meta: '182 tok' },
  ]);
  const [sessions] = useState(['Tax calculation', 'Report CSV cleanup', 'Audit dependency tree']);
  const [title, setTitle] = useState('Find why calc_total uses 8% tax');
  const [composer, setComposer] = useState('');
  const [mode, setMode] = useState<Mode>('ask');
  const [theme, setTheme] = useState<'light' | 'dark'>('light');
  const [approval, setApproval] = useState<'pending' | 'allowed' | 'denied'>('pending');
  const [running, setRunning] = useState(false);
  const [palette, setPalette] = useState(false);
  const [cache, setCache] = useState(0);
  const [resultCap, setResultCap] = useState<number | null>(null);
  const [workspace, setWorkspace] = useState('C:/Users/parag/Downloads/tinyloop');
  const [verifyCommand, setVerifyCommand] = useState('cargo test --lib');

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  // Prove the Tauri bridge is live: the cap comes from tinyloop-core, not a hardcoded UI value.
  useEffect(() => {
    if (!('__TAURI_INTERNALS__' in window)) return;
    import('@tauri-apps/api/core')
      .then(({ invoke }) => invoke<number>('context_budget'))
      .then(setResultCap)
      .catch(() => setResultCap(null));
  }, []);

  // In the packaged app, core → UI events stream through Tauri.
  // In a browser preview this guard is false and the prototype simulation runs.
  useEffect(() => {
    if (!isTauri) return;
    let off: (() => void) | undefined;
    import('@tauri-apps/api/event').then(({ listen }) => {
      listen<UiEvent>('run_event', event => {
        const e = event.payload;
        setSteps(s => [...s, { id: Date.now() + Math.random(), title: e.title, detail: e.detail, status: e.status, meta: e.meta }]);
      }).then(fn => { off = fn; });
    });
    return () => off?.();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); setPalette(p => !p); }
      if (e.key === 'Escape') setPalette(false);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'd') { e.preventDefault(); setTheme(t => t === 'dark' ? 'light' : 'dark'); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const add = (s: Omit<Step, 'id'>) => setSteps(prev => [...prev, { ...s, id: Date.now() + Math.random() }]);

  const run = async () => {
    if (running) return;
    setRunning(true);
    const text = composer.trim() || 'Run sample: repo tax investigation';
    setTitle(text); setComposer(''); setSteps([]);
    if (isTauri) {
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        const report = await invoke<{ status?: string; steps: number; prompt_tokens: number; completion_tokens: number; cache_hits: number }>('run_task', {
          task: text,
          doneCondition: 'the verifier command exits 0',
          boundaries: ['do not edit test files', 'stay inside the workspace', 'no network'],
          verifyCommand,
          cwd: workspace,
          mode,
          model: 'spark-x26',
        });
        setCache(report.cache_hits);
        setApproval(report.status === 'Done' ? 'allowed' : 'denied');
      } catch (error) {
        add({ title: 'Error', detail: String(error).slice(0, 400), status: 'blocked', meta: 'core' });
        setApproval('denied');
      }
      setRunning(false);
      return;
    }
    add({ title: 'Plan', detail: 'submit_plan(3 steps)', status: 'done', meta: '0.4s' });
    await sleep(360);
    add({ title: 'Act', detail: 'read_file src/tax.rs', status: 'done', meta: '182 tok' });
    await sleep(420);
    add({ title: 'Act', detail: 'search_files pattern="8%"', status: 'done', meta: '46 tok' });
    setCache(c => c + 2);
    await sleep(420);
    add({ title: 'Verify', detail: 'cargo test --quiet · independent judge', status: 'done', meta: 'verified' });
    setApproval('allowed'); setRunning(false);
  };

  const mark = (s: Status) => s === 'done' ? '✓' : s === 'running' ? '⟳' : s === 'blocked' ? '!' : '·';

  return (
    <div className="desk"><div className="window">
      <header className="titlebar">
        <div className="traffic"><i className="dot r"/><i className="dot y"/><i className="dot g"/></div>
        <div className="brand"><span className="glyph">t</span>tinyloop</div>
        <span className="badge">Tauri shell</span>
        <div className="spacer"/>
        <div className="segmented">{(['ask','auto','dry'] as Mode[]).map(m => (
          <button key={m} aria-pressed={mode === m} onClick={() => setMode(m)}>{m === 'dry' ? 'Dry run' : m[0].toUpperCase() + m.slice(1)}</button>
        ))}</div>
        <div className="pill"><i className="live"/>Spark X2.6 · 4B · 8K</div>
        <button className="icon-btn" aria-label="Appearance" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}>◐</button>
        <button className="run-btn" disabled={running} onClick={run}>▶ Run</button>
      </header>

      <div className="body">
        <aside className="sidebar">
          <div><div className="side-label">Sessions</div>
            {sessions.map((s, i) => <button className="session" aria-selected={i === 0} key={s}>
              <span className="s-icon">{i + 1}</span><span><span className="s-title">{s}</span><span className="s-meta">bounded loop</span></span>
            </button>)}</div>
          <button className="new-btn" onClick={() => setPalette(true)}>＋ New task</button>
          <div className="side-foot"><div className="side-label">Model</div>
            <div className="metric"><span>Cache hits</span><b>{cache}</b></div>
            <div className="metric"><span>Context</span><b>8192</b></div>
            <div className="metric"><span>Result cap</span><b>{resultCap ? resultCap.toLocaleString() : '…'} ch</b></div></div>
        </aside>

        <main className="main">
          <div className="main-head"><div className="crumb">Sessions / <span>current run</span></div>
            <h1>{title}</h1><div className="sub">Bounded loop · one tool call per turn · deterministic verifier</div></div>
          <div className="timeline"><AnimatePresence initial={false}>
            {steps.map(s => <motion.div key={s.id} className={'step ' + s.status} initial={{ opacity: 0, y: 8, filter: 'blur(3px)' }} animate={{ opacity: 1, y: 0, filter: 'none' }} exit={{ opacity: 0 }}>
              <div className="mark">{mark(s.status)}</div>
              <div><div className="s-title">{s.title}</div><div className="s-detail">{s.detail}</div></div>
              <div className="s-meta">{s.meta}</div>
            </motion.div>)}
          </AnimatePresence></div>
          <div className="composer"><div className="field">
            <textarea rows={1} value={composer} placeholder="Describe a task for the agent…" onChange={e => setComposer(e.target.value)}
              onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void run(); } }} />
            <button className="send" aria-label="Send" onClick={() => void run()}>↑</button>
          </div><div className="hint"><span><kbd>↵</kbd> send</span><span><kbd>⌘K</kbd> commands</span></div></div>
        </main>

        <aside className="inspector">
          {isTauri && <div className="section"><h2>Run setup</h2><div className="contract">
            <label className="k">Workspace</label>
            <input className="run-input" value={workspace} onChange={e => setWorkspace(e.target.value)} />
            <label className="k">Verifier</label>
            <input className="run-input" value={verifyCommand} onChange={e => setVerifyCommand(e.target.value)} />
            <p className="k" style={{ marginTop: 6 }}>Ask mode blocks writes until approval is wired; use Auto for a full run.</p>
          </div></div>}
          <div className="section"><h2>Goal</h2><div className="contract"><p><span className="k">Done when</span><code>cargo test</code> exits 0 and <code>src/tax.rs</code> contains <code>fn calc_total</code></p></div></div>
          <div className="section"><h2>Boundaries</h2><div className="contract"><p><span className="k">Must not</span> edit tests, touch files outside <code>src/</code>, or use the network.</p></div></div>
          <div className="section"><h2>Verifier</h2><div className="contract"><p><code>cargo test --quiet</code><br/><span className="k">Judge</span><code>Rust, not the model</code></p></div></div>
          <div className="divider"/>
          <div className="section"><h2>Approval</h2>
            {approval === 'pending' ? <div className="approval"><div className="head"><i className="dot2"/>Approval required</div>
              <div className="cmd">apply_patch src/tax.rs</div><div className="hash">bound to sha256:9f2a…c41d</div>
              <div className="actions"><button className="btn primary" onClick={() => setApproval('allowed')}>Allow</button>
                <button className="btn quiet" onClick={() => setApproval('denied')}>Deny</button></div></div>
              : <div className="resolved"><span className="ok">{approval === 'allowed' ? '✓' : '×'}</span> {approval === 'allowed' ? 'Approved and bound' : 'Denied — no changes made'}</div>}
          </div>
        </aside>
      </div>

      {palette && <div className="palette open" onClick={() => setPalette(false)}><div className="pbox" onClick={e => e.stopPropagation()}>
        <input placeholder="Type a command…" autoFocus aria-label="Command"/>
        <button className="pitem" onClick={() => setPalette(false)}><span>New task</span><span>⌘N</span></button>
        <button className="pitem" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}><span>Toggle appearance</span><span>⌘D</span></button>
        <button className="pitem" onClick={() => { setMode('dry'); setPalette(false); }}><span>Dry-run mode</span><span>⌘⇧D</span></button>
      </div></div>}
    </div></div>
  );
}
