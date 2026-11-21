// Opens our own profile with the avatar cropper on a portrait picture.
(() => {
    openProfile();
    setTimeout(() => VectorSvelte.openCropper({ src: '/__ref/media/portrait.png', aspect: 1, title: 'Crop Profile Picture', hint: 'Drag to move, or drag a corner to resize.', shape: 'round' }), 300);
})();
