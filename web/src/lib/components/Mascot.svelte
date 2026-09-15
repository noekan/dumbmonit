<script lang="ts">
	/**
	 * The DumbMonit pigeon: round, slate-blue, googly eyes, orange beak and feet,
	 * a green chest patch. Drawn as geometry so it stays crisp at 16px and 400px.
	 * `mood` moves the pupils: "watch" looks at you, "dizzy" crosses them.
	 */
	interface Props {
		class?: string;
		mood?: 'watch' | 'dizzy' | 'happy';
	}
	let { class: className = 'size-16', mood = 'watch' }: Props = $props();

	const pupils = $derived(
		mood === 'dizzy'
			? { l: [104, 134], r: [176, 108] }
			: mood === 'happy'
				? { l: [96, 122], r: [184, 122] }
				: { l: [82, 124], r: [190, 114] }
	);
</script>

<svg viewBox="0 0 280 320" class={className} role="img" aria-label="DumbMonit, the pigeon">
	<!-- feet -->
	<path d="M96 266h30v20c0 8-6 14-14 14H86c-7 0-11-6-9-12 3-11 10-22 19-22zM184 266h-30v20c0 8 6 14 14 14h26c7 0 11-6 9-12-3-11-10-22-19-22z" fill="#e97b3a" stroke="#1e2640" stroke-width="12" stroke-linejoin="round" />
	<!-- body -->
	<path d="M140 46c70 0 118 54 118 128 0 62-44 104-118 104S22 236 22 174C22 100 70 46 140 46z" fill="#6f83a3" stroke="#1e2640" stroke-width="12" />
	<!-- wings (lobes inside the outline) -->
	<path d="M40 150c-14 40-10 90 18 118" fill="none" stroke="#1e2640" stroke-width="11" stroke-linecap="round" />
	<path d="M240 150c14 40 10 90-18 118" fill="none" stroke="#1e2640" stroke-width="11" stroke-linecap="round" />
	<!-- chest patch -->
	<path d="M78 190c20 22 46 34 78 34 26 0 44-8 54-20-6 30-34 52-70 52-34 0-56-26-62-66z" fill="#4c9d6f" />
	<path d="M110 232c30 2 52-8 64-26" fill="none" stroke="#1e2640" stroke-width="10" stroke-linecap="round" />
	<!-- eyes -->
	<circle cx="96" cy="118" r="52" fill="#fff" stroke="#1e2640" stroke-width="12" />
	<circle cx="184" cy="120" r="42" fill="#fff" stroke="#1e2640" stroke-width="12" />
	<circle cx={pupils.l[0]} cy={pupils.l[1]} r="19" fill="#1e2640" />
	<circle cx={pupils.l[0] + 8} cy={pupils.l[1] - 8} r="6" fill="#fff" />
	<circle cx={pupils.r[0]} cy={pupils.r[1]} r="15" fill="#1e2640" />
	<!-- beak -->
	<path d="M114 150c8-10 44-10 52 0l-26 46z" fill="#e97b3a" stroke="#1e2640" stroke-width="10" stroke-linejoin="round" />
	<path d="M120 152c6-6 34-6 40 0" fill="none" stroke="#fff" stroke-width="7" stroke-linecap="round" />
</svg>
