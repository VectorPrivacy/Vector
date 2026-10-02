<script>
    // A community banner at the template's proportions, 238 x 146, its top band fading into
    // whatever sits above it. `rect` (source pixels, with the image's `natural` size) shows a
    // crop before it is made; without one the image is centred and cut to fit, as a banner
    // from another client is.
    let { src, rect = null, natural = null, class: cls = '', children = undefined } = $props();

    let width = $state(0);
    const background = $derived.by(() => {
        if (!src) return '';
        const image = `background-image: url("${src}");`;
        if (!rect || !natural?.w || !width) return `${image} background-size: cover; background-position: center;`;
        const k = width / rect.w;
        return `${image} background-size: ${natural.w * k}px ${natural.h * k}px; background-position: ${-rect.x * k}px ${-rect.y * k}px;`;
    });
</script>

<div class="banner-art {cls}" bind:clientWidth={width} style={background}>
    {@render children?.()}
</div>
