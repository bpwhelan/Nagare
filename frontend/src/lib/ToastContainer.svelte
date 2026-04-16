<script>
  import { toasts, showToast } from './stores.js';
</script>

<div class="toast-container">
  {#each $toasts as toast (toast.id)}
    <div class="toast" class:success={toast.type === 'success'} class:error={toast.type === 'error'}>
      <span class="toast-icon">{toast.type === 'success' ? '✓' : '✕'}</span>
      <span class="toast-message">{toast.message}</span>
      <button class="toast-close" on:click={() => toasts.update(ts => ts.filter(t => t.id !== toast.id))}>×</button>
    </div>
  {/each}
</div>

<style>
  .toast-container {
    position: fixed;
    bottom: 1.5rem;
    right: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    z-index: 2000;
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.65rem 1rem;
    border-radius: 8px;
    font-size: 0.9rem;
    min-width: 220px;
    max-width: 400px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
    pointer-events: all;
    animation: slide-in 0.2s ease;
  }

  @keyframes slide-in {
    from { opacity: 0; transform: translateX(1rem); }
    to   { opacity: 1; transform: translateX(0); }
  }

  .toast.success {
    background: #1e3a2a;
    border: 1px solid #4caf50;
    color: #a5d6a7;
  }

  .toast.error {
    background: #3a1e1e;
    border: 1px solid var(--accent);
    color: #ef9a9a;
  }

  .toast-icon {
    flex-shrink: 0;
    font-weight: 700;
  }

  .toast-message {
    flex: 1;
    word-break: break-word;
  }

  .toast-close {
    flex-shrink: 0;
    background: none;
    border: none;
    color: inherit;
    font-size: 1.1rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0.7;
    padding: 0 0.1rem;
  }

  .toast-close:hover {
    opacity: 1;
  }

  /* ── Mobile ── */
  @media (max-width: 768px) {
    .toast-container {
      right: 0.5rem;
      left: 0.5rem;
      bottom: 0.75rem;
    }

    .toast {
      min-width: unset;
      max-width: 100%;
      font-size: 0.85rem;
    }
  }
</style>
