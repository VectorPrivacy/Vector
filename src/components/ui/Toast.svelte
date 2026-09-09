<script>
    import { toastState } from '../lib/toast.svelte.js';
    const t = toastState();
</script>

<div class="toast-backdrop" class:visible={t.visible}></div>
<div class="app-toast" class:visible={t.visible}>{t.text}</div>

<style>
    .app-toast {
        position: fixed;
        bottom: 80px;
        left: 50%;
        transform: translateX(-50%);
        background: rgba(0, 0, 0, 0.8);
        backdrop-filter: blur(20px);
        -webkit-backdrop-filter: blur(10px);
        border: 1px solid var(--toast-border-color, #161616);
        box-shadow: 0 0 4px rgba(0, 0, 0, 0.8), 0 0 12px rgba(0, 0, 0, 0.6), 0 0 30px rgba(0, 0, 0, 0.4);
        color: white;
        padding: 12px 24px;
        border-radius: 8px;
        z-index: 10000;
        font-size: 14px;
        opacity: 0;
        transition: opacity 0.3s ease;
        pointer-events: none;
    }
    .toast-backdrop {
        position: fixed;
        top: 0; left: 0; width: 100%; height: 100%;
        background: linear-gradient(to bottom, rgba(0,0,0,0) 0%, rgba(0,0,0,0) 50%, rgba(0,0,0,0.8) 100%);
        opacity: 0;
        z-index: 9999;
        pointer-events: none;
        transition: opacity 0.3s ease;
    }
    .app-toast.visible, .toast-backdrop.visible { opacity: 1; }
</style>
