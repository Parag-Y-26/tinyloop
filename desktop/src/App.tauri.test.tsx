import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import type { ComponentType } from 'react';

type Listener = (event: { payload: unknown }) => void;

const { invokeMock, listeners } = vi.hoisted(() => ({
  invokeMock: vi.fn<(cmd: string, args?: Record<string, unknown>) => Promise<unknown>>(),
  listeners: {} as Record<string, Listener>,
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: async (name: string, cb: Listener) => {
    listeners[name] = cb;
    return () => { delete listeners[name]; };
  },
}));

let App: ComponentType;

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
};

const report = (status = 'Done') => ({
  status,
  steps: 4,
  prompt_tokens: 3924,
  completion_tokens: 139,
  cache_hits: 3535,
});

const approval = {
  id: 'req-1',
  tool: 'patch_file',
  args: { path: 'src/tax.rs' },
  hash: 'abcdef1234567890',
};

beforeEach(async () => {
  vi.clearAllMocks();
  for (const key of Object.keys(listeners)) delete listeners[key];
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === 'context_budget') return Promise.resolve(12000);
    if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
    return Promise.resolve(true);
  });
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  ({ default: App } = await import('./App'));
});

const startRun = async () => {
  render(<App />);
  fireEvent.click(screen.getByTestId('run-button'));
  fireEvent.click(screen.getByTestId('clarify-confirm'));
  await waitFor(() => expect(listeners['approval_required']).toBeDefined());
};

describe('tinyloop desktop under Tauri', () => {
  it('blocks a write until Allow and forwards the exact approval id', async () => {
    const run = deferred<ReturnType<typeof report>>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'context_budget') return Promise.resolve(12000);
      if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
    if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
      if (cmd === 'run_task') return run.promise;
      return Promise.resolve(true);
    });

    await startRun();
    expect(invokeMock).toHaveBeenCalledWith('run_task', expect.objectContaining({ mode: 'ask' }));

    act(() => { listeners['approval_required']({ payload: approval }); });
    expect(screen.getByTestId('approval-card')).toBeInTheDocument();
    expect(screen.getByText(/patch_file/)).toBeInTheDocument();
    expect(screen.getByText(/sha256:abcdef12/)).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('allow-button'));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('approve', { id: 'req-1' }));
    expect(screen.queryByTestId('approval-card')).not.toBeInTheDocument();
    expect(screen.getByText('Approved and bound')).toBeInTheDocument();

    await act(async () => { run.resolve(report()); });
    await waitFor(() => expect(screen.queryByLabelText('Cancel run')).not.toBeInTheDocument());
  });

  it('denies a write without touching the file and reports the denial', async () => {
    const run = deferred<ReturnType<typeof report>>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'context_budget') return Promise.resolve(12000);
      if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
    if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
      if (cmd === 'run_task') return run.promise;
      return Promise.resolve(true);
    });

    await startRun();
    act(() => { listeners['approval_required']({ payload: approval }); });
    fireEvent.click(screen.getByTestId('deny-button'));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('deny', { id: 'req-1' }));
    expect(screen.getByText('Denied — no changes made')).toBeInTheDocument();
    expect(screen.queryByTestId('approval-card')).not.toBeInTheDocument();

    await act(async () => { run.resolve(report('Denied')); });
  });

  it('sends the clarified contract, workspace, verifier and mode to the core', async () => {
    render(<App />);

    fireEvent.change(screen.getByTestId('task-input'), { target: { value: 'fix the tax bug' } });
    fireEvent.change(screen.getByTestId('workspace-input'), { target: { value: 'C:/work/repo' } });
    fireEvent.change(screen.getByTestId('verifier-input'), { target: { value: 'cargo test --quiet' } });
    fireEvent.click(screen.getByRole('button', { name: 'Auto' }));
    fireEvent.click(screen.getByTestId('run-button'));
    fireEvent.click(screen.getByTestId('clarify-confirm'));

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('run_task', {
      task: 'fix the tax bug',
      doneCondition: 'the verifier command exits 0',
      boundaries: ['do not edit test files', 'stay inside the workspace', 'no network'],
      verifyCommand: 'cargo test --quiet',
      cwd: 'C:/work/repo',
      mode: 'auto',
      model: 'spark-x26',
    }));
  });

  it('cancels a running loop through the core', async () => {
    const run = deferred<ReturnType<typeof report>>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'context_budget') return Promise.resolve(12000);
      if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
    if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
      if (cmd === 'run_task') return run.promise;
      return Promise.resolve(true);
    });

    await startRun();
    const cancel = await screen.findByLabelText('Cancel run');
    fireEvent.click(cancel);

    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('cancel_run', undefined));
    await waitFor(() => expect(screen.queryByLabelText('Cancel run')).not.toBeInTheDocument());
  });

  it('surfaces a core failure as a blocked step and a denial', async () => {
    const run = deferred<ReturnType<typeof report>>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'context_budget') return Promise.resolve(12000);
      if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
    if (cmd === 'current_dir') return Promise.resolve('C:/work/repo');
      if (cmd === 'run_task') return run.promise;
      return Promise.resolve(true);
    });

    await startRun();
    await act(async () => { run.reject(new Error('connection refused')); });

    await waitFor(() => expect(screen.getByText(/connection refused/)).toBeInTheDocument());
    expect(screen.getByText('Denied — no changes made')).toBeInTheDocument();
  });
});
