<script>
    // Six one-digit boxes, filled strictly left to right: focus anywhere lands on the next box to
    // fill, a digit always goes there, a pasted PIN fills on from there, Backspace clears then
    // steps back, anything else is dropped. The full PIN is reported once every box holds a digit.
    import { untrack } from 'svelte';
    let { id = undefined, cls = 'row pin-row', inputIds = null, inputClass = undefined,
          resetSeq = 0, focusOnReset = true, onFull, onBackspace = null } = $props();
    const slots = [0, 1, 2, 3, 4, 5];
    let inputs = $state([]);
    // The box the next digit goes into: the first empty one, or the last once all are full.
    // Only it takes Tab, so the row is one stop and Tab still leaves it.
    let next = $state(0);
    const sync = () => {
        const empty = inputs.findIndex((i) => !i?.value);
        next = empty < 0 ? slots.length - 1 : empty;
    };
    // Moving between boxes must not scroll: iOS would reveal each box anew, a bounce per digit.
    const focus = (el) => el?.focus({ preventScroll: true });

    export function clear(focusFirst = true) {
        for (const el of inputs) if (el) el.value = '';
        sync();
        if (focusFirst) focus(inputs[0]);
    }
    export function focusFirst() { focus(inputs[next]); }
    export function focusNext() { focus(inputs[next]); }
    $effect(() => { resetSeq; untrack(() => clear(focusOnReset)); });

    function keydown(e, n) {
        const input = inputs[n];
        if (e.key === 'Backspace') {
            e.preventDefault();
            onBackspace?.();
            if (input.value !== '') input.value = '';
            else if (n > 0) inputs[n - 1].value = '';
            sync();
            focus(inputs[next]);
        } else if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
            e.preventDefault();
        } else if (e.key.length === 1 && !/^[0-9]$/.test(e.key)) {
            e.preventDefault();
        }
    }
    function report() {
        const digits = inputs.map(i => i?.value || '');
        if (digits.every(d => /^[0-9]$/.test(d))) onFull(digits.join(''));
    }
    /** Lays `text`'s digits into the empty boxes, left to right. */
    function spread(text) {
        let placed = false;
        for (const d of text.replace(/[^0-9]/g, '')) {
            sync();
            if (inputs[next].value) break;
            inputs[next].value = d;
            placed = true;
        }
        sync();
        focus(inputs[next]);
        if (placed) report();
    }
    function input(n) {
        const el = inputs[n];
        const v = el.value.replace(/[^0-9]/g, '').charAt(0);
        el.value = '';
        spread(v);
    }
    // maxlength would cut a pasted PIN to its first digit, so pastes are taken whole: from the
    // clipboard, or a keyboard's suggestion strip, which inserts without a paste event.
    function paste(e) {
        e.preventDefault();
        spread(e.clipboardData?.getData('text') || '');
    }
    function beforeinput(e) {
        if (e.inputType === 'insertText' && e.data && e.data.length > 1) {
            e.preventDefault();
            spread(e.data);
        }
    }
</script>

<div {id} class={cls}>
    {#each slots as n (n)}
        <input type="password" inputmode="numeric" maxlength="1" id={inputIds ? inputIds[n] : undefined} class={inputClass}
               tabindex={n === next ? 0 : -1} aria-label={`PIN digit ${n + 1} of ${slots.length}`}
               bind:this={inputs[n]} onfocus={() => { if (n !== next) focus(inputs[next]); }}
               onkeydown={(e) => keydown(e, n)} oninput={() => input(n)} onpaste={paste} onbeforeinput={beforeinput}>
    {/each}
</div>
