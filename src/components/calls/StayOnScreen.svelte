<script>
    // Vector Web on an iPhone or iPad loses the microphone whenever it leaves the screen. Only
    // their WebKit has touch callouts, whatever a Mac's screen can do.
    const pausesHidden = document.documentElement.classList.contains('vector-web') && CSS.supports('-webkit-touch-callout', 'none');
</script>

{#if pausesHidden}
    <div class="call-pill-hint"><span class="icon icon-warning"></span>Keep Vector on screen to stay heard</div>
{/if}
