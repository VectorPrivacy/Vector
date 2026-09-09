// The one global tooltip: its text and the rect it points at. The component measures
// itself to centre and clamp, so callers hand over the target's rect, not a position.
const tip = $state({ text: '', visible: false, rect: null });
export function tooltipState() { return tip; }
export function showTooltip(text, rect) {
    tip.text = text;
    tip.rect = { left: rect.left, top: rect.top, width: rect.width };
    tip.visible = true;
}
export function hideTooltip() { tip.visible = false; }
