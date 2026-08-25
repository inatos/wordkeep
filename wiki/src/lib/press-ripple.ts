/** Compact press ripple for Wordkeep wiki sidebar / tree clicks. */

export function spawnPressRipple(clientX: number, clientY: number): void {
	if (typeof document === 'undefined') return;
	if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;

	const host = document.createElement('span');
	host.className = 'press-ripple';
	host.setAttribute('aria-hidden', 'true');
	host.style.left = `${clientX}px`;
	host.style.top = `${clientY}px`;

	const core = document.createElement('span');
	core.className = 'press-ripple-core';
	const ring = document.createElement('span');
	ring.className = 'press-ripple-ring';

	host.append(core, ring);
	document.body.appendChild(host);

	const clear = () => host.remove();
	host.addEventListener('animationend', clear, { once: true });
	window.setTimeout(clear, 720);
}

export function rippleFromEvent(event: MouseEvent): void {
	spawnPressRipple(event.clientX, event.clientY);
}
