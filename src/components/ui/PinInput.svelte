<script>
    // Six one-digit boxes: digits advance, Backspace clears then steps back, arrows move,
    // anything else is dropped. The full PIN is reported once every box holds a digit.
    import { untrack } from 'svelte';
    let { id = undefined, cls = 'row pin-row', inputIds = null, inputClass = undefined,
          resetSeq = 0, focusOnReset = true, onFull, onBackspace = null } = $props();
    const slots = [0, 1, 2, 3, 4, 5];
    let inputs = $state([]);
    // Moving between boxes must not scroll: iOS would reveal each box anew, a bounce per digit.
    const focus = (el) => el?.focus({ preventScroll: true });

    export function clear(focusFirst = true) {
        for (const el of inputs) if (el) el.value = '';
        if (focusFirst) focus(inputs[0]);
    }
    export function focusFirst() { focus(inputs[0]); }
    export function focusNext() { focus(inputs.find((i) => !i.value) || inputs[inputs.length - 1]); }
    $effect(() => { resetSeq; untrack(() => clear(focusOnReset)); });

    function keydown(e, n) {
        const input = inputs[n];
        if (e.key === 'Backspace') {
            e.preventDefault();
            onBackspace?.();
            if (input.value !== '') input.value = '';
            else if (n > 0) { inputs[n - 1].value = ''; focus(inputs[n - 1]); }
        } else if (e.key === 'ArrowLeft') {
            e.preventDefault();
            if (n > 0) focus(inputs[n - 1]);
        } else if (e.key === 'ArrowRight') {
            e.preventDefault();
            if (n + 1 < inputs.length) focus(inputs[n + 1]);
        } else if (e.key.length === 1 && !/^[0-9]$/.test(e.key)) {
            e.preventDefault();
        }
    }
    function input(n) {
        const el = inputs[n];
        let v = el.value.replace(/[^0-9]/g, '');
        if (v.length > 1) v = v.charAt(0);
        el.value = v;
        if (v && n + 1 < inputs.length) focus(inputs[n + 1]);
        const digits = inputs.map(i => i?.value || '');
        if (digits.every(d => /^[0-9]$/.test(d))) onFull(digits.join(''));
    }
</script>

<div {id} class={cls}>
    {#each slots as n (n)}
        <input type="password" inputmode="numeric" maxlength="1" id={inputIds ? inputIds[n] : undefined} class={inputClass}
               bind:this={inputs[n]} onkeydown={(e) => keydown(e, n)} oninput={() => input(n)}>
    {/each}
</div>
