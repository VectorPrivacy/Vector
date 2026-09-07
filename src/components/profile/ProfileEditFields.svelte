<script>
    // The Edit Mode fields, bound to the draft. Status is set from the profile itself. Values go through DOM properties,
    // never markup.
    import { profileEdit } from '../lib/profileedit.svelte.js';

    const edit = profileEdit();
    const field = 'background: none; border: none; outline: none; color: inherit; font-size: 16px; width: 100%;';

    // The bio grows with its text. It is measured after a tick because the fields are
    // shown in the same flush that mounts Edit Mode; a hidden textarea measures 0.
    function autosize(el) {
        const fit = () => { el.style.height = 'auto'; el.style.height = el.scrollHeight + 'px'; };
        const soon = () => setTimeout(fit, 10);
        soon();
        el.addEventListener('input', fit);
        return { update: soon };
    }
</script>

<label class="profile-edit-label" for="profile-edit-name-input">Username</label>
<div class="profile-edit-field-wrapper" style="position: relative;">
    <div id="profile-edit-name" class="profile-edit-field-text"><input id="profile-edit-name-input" type="text" maxlength="50" style={field} bind:value={edit.draft.name}></div>
</div>
<label class="profile-edit-label" for="profile-edit-bio-input">Bio</label>
<div class="profile-edit-field-wrapper profile-edit-field-bio">
    <div id="profile-edit-bio" class="profile-edit-field-text"><textarea id="profile-edit-bio-input" style="{field} resize: none; min-height: 60px;" bind:value={edit.draft.about} use:autosize={[edit.draft.about, edit.active]}></textarea></div>
</div>
