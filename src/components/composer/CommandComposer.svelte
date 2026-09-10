<script>
    // The command composer's argument pills. One pill per argument:
    // a picker trigger for choice and bool, a growing textarea for free text, an
    // input otherwise. The controller in command-picker.js attaches each field's
    // behaviour (key walking, filtering, sizing) through `attach`, so the keyboard
    // flow that was device-tested stays where it was.
    import { composerCommand } from '../lib/composer.svelte.js';

    const command = composerCommand();

    function field(node, idx) {
        const cleanup = command.attach ? command.attach(node, idx) : null;
        return { destroy: () => { if (cleanup) cleanup(); } };
    }
</script>

{#key command.seq}
    {#if command.active}
        <div class="command-composer">
            {#each command.args as arg, idx (idx)}
                <label
                    class="command-part"
                    class:required={arg.required}
                    class:grow={arg.grow}
                    class:invalid={command.invalid === idx}
                >
                    <span class="command-part-name">{arg.name}</span>
                    {#if arg.type === 'choice' || arg.type === 'bool'}
                        <!-- A trigger that dresses as a field: native selects render inconsistently
                             per platform and cannot join the composer's keyboard flow. The "…"
                             glyph is the unset state; the pill's name carries the semantics. -->
                        <button
                            type="button"
                            class="command-choice-trigger command-part-input"
                            class:placeholder={!command.values[idx]}
                            value={command.values[idx]}
                            title={command.values[idx] || arg.description}
                            use:field={idx}
                        ><span>{command.values[idx] || '…'}</span></button>
                    {:else if arg.type === 'string'}
                        <textarea
                            class="command-part-input"
                            rows="1"
                            maxlength="1024"
                            autocomplete="off"
                            spellcheck="true"
                            title={arg.description}
                            use:field={idx}
                        ></textarea>
                    {:else}
                        <input
                            type="text"
                            class="command-part-input"
                            inputmode={arg.type === 'int' || arg.type === 'number' ? 'decimal' : undefined}
                            placeholder={arg.type === 'user' ? 'npub1…' : undefined}
                            maxlength={arg.type === 'user' ? 70 : 1024}
                            autocomplete="off"
                            spellcheck="false"
                            title={arg.description}
                            use:field={idx}
                        />
                    {/if}
                </label>
            {/each}
        </div>
    {/if}
{/key}
