import { describe, it, expect } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import App from './App';

describe('tinyloop desktop shell', () => {
  it('requires the clarify contract before running', async () => {
    render(<App />);
    fireEvent.click(screen.getByText('▶ Run'));
    expect(screen.getByText('Confirm the goal')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Confirm & Run'));
    await waitFor(() => expect(screen.getByText('Verify')).toBeInTheDocument(), { timeout: 5000 });
  });

  it('toggles appearance', () => {
    render(<App />);
    fireEvent.click(screen.getByLabelText('Appearance'));
    expect(document.documentElement.dataset.theme).toBe('dark');
  });
});
