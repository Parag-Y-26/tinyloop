import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import App from './App';

describe('tinyloop desktop shell (browser preview)', () => {
  it('requires the clarify contract before running', async () => {
    render(<App />);
    fireEvent.click(screen.getByTestId('run-button'));
    expect(screen.getByText('Confirm the goal')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('clarify-confirm'));
    await waitFor(() => expect(screen.getByText('Verify')).toBeInTheDocument(), { timeout: 5000 });
    expect(screen.getByText('Approved and bound')).toBeInTheDocument();
  });

  it('toggles appearance', () => {
    render(<App />);
    fireEvent.click(screen.getByLabelText('Appearance'));
    expect(document.documentElement.dataset.theme).toBe('dark');
  });

  it('switches run mode', () => {
    render(<App />);
    expect(screen.getByRole('button', { name: 'Ask' })).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'Auto' }));
    expect(screen.getByRole('button', { name: 'Auto' })).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'Dry run' }));
    expect(screen.getByRole('button', { name: 'Dry run' })).toHaveAttribute('aria-pressed', 'true');
  });

  it('opens the command palette with the keyboard and closes on Escape', () => {
    render(<App />);
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    expect(screen.getByPlaceholderText('Type a command…')).toBeInTheDocument();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.queryByPlaceholderText('Type a command…')).not.toBeInTheDocument();
  });

  it('drives the prototype timeline from composer to verified state', async () => {
    render(<App />);
    fireEvent.change(screen.getByTestId('task-input'), { target: { value: 'fix the tax bug' } });
    fireEvent.click(screen.getByTestId('run-button'));
    fireEvent.click(screen.getByTestId('clarify-confirm'));
    await waitFor(() => expect(screen.getAllByText('Act').length).toBeGreaterThanOrEqual(2), { timeout: 5000 });
    await waitFor(() => expect(screen.getByText('Verify')).toBeInTheDocument(), { timeout: 5000 });
    expect(screen.getByText('Approved and bound')).toBeInTheDocument();
  });
});
