/**
 * Markdown → HTML for the built-in documentation, with `marked`.
 *
 * Two things matter here:
 * - headings get a GitHub-style `id` (lowercase, accents kept, punctuation
 *   dropped, spaces → `-`) so the server's `doc_url` anchors
 *   (`/docs/notifications#courriel-smtp`) and the document's own table of
 *   contents land on the right section;
 * - the HTML carries its token classes directly, so the article follows both
 *   themes without a typography plugin.
 */
import { Marked, type Tokens } from 'marked';

export interface TocEntry {
	id: string;
	text: string;
	depth: number;
}

export interface RenderedDoc {
	html: string;
	toc: TocEntry[];
}

/** "Courriel (SMTP)" → `courriel-smtp`; "Bark (iOS)" → `bark-ios`. */
export function slugify(text: string): string {
	return text
		.toLowerCase()
		.trim()
		.replace(/[^\p{L}\p{N}\s-]/gu, '')
		.replace(/\s/g, '-');
}

/** Plain text of a heading, without inline markup (`code`, **bold**…). */
function plainText(tokens: Tokens.Generic[]): string {
	return tokens
		.map((token) => {
			if ('tokens' in token && Array.isArray(token.tokens)) return plainText(token.tokens);
			return typeof token.text === 'string' ? token.text : '';
		})
		.join('');
}

const HEADING_CLASS: Record<number, string> = {
	1: 'display text-3xl text-ink sm:text-4xl',
	2: 'mt-14 border-b border-line pb-2 text-xl font-semibold tracking-tight text-ink',
	3: 'mt-10 text-lg font-semibold tracking-tight text-ink',
	4: 'mt-7 text-base font-semibold text-ink'
};

const LINK_CLASS = 'font-medium text-signal-ink underline decoration-signal/40 hover:decoration-signal';

/** Escapes just what an attribute or text node needs. */
function escape(text: string): string {
	return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

export function renderMarkdown(source: string): RenderedDoc {
	const marked = new Marked({ gfm: true });
	const tokens = marked.lexer(source);

	// Anchors are assigned in one pass, in document order, so the table of
	// contents and the rendered headings share the same ids, duplicates included.
	const ids = new WeakMap<Tokens.Heading, string>();
	const seen = new Map<string, number>();
	const toc: TocEntry[] = [];
	for (const token of tokens) {
		if (token.type !== 'heading') continue;
		const heading = token as Tokens.Heading;
		const base = slugify(plainText(heading.tokens)) || 'section';
		const n = seen.get(base) ?? 0;
		seen.set(base, n + 1);
		const id = n === 0 ? base : `${base}-${n}`;
		ids.set(heading, id);
		if (heading.depth >= 2 && heading.depth <= 3) {
			toc.push({ id, text: plainText(heading.tokens), depth: heading.depth });
		}
	}

	marked.use({
		renderer: {
			heading(token) {
				const id = ids.get(token) ?? slugify(plainText(token.tokens));
				const classes = HEADING_CLASS[token.depth] ?? HEADING_CLASS[4];
				const inner = this.parser.parseInline(token.tokens);
				return `<h${token.depth} id="${escape(id)}" class="group scroll-mt-24 ${classes}">${inner} <a href="#${escape(id)}" class="ml-1 text-ink-3 no-underline opacity-0 transition-opacity group-hover:opacity-100 focus:opacity-100" aria-label="Link to this section">#</a></h${token.depth}>\n`;
			},
			paragraph(token) {
				return `<p class="my-4 leading-relaxed text-ink">${this.parser.parseInline(token.tokens)}</p>\n`;
			},
			blockquote(token) {
				return `<blockquote class="my-5 rounded-lg border border-signal/30 bg-signal-soft px-4 py-1 text-ink">${this.parser.parse(token.tokens)}</blockquote>\n`;
			},
			code(token) {
				return `<pre class="my-4 overflow-x-auto rounded-lg border border-line bg-canvas-deep px-4 py-3 font-mono text-[0.8125rem] leading-relaxed text-ink"><code>${token.escaped ? token.text : escape(token.text)}</code></pre>\n`;
			},
			codespan(token) {
				return `<code class="rounded-md border border-line bg-canvas-deep px-1.5 py-0.5 font-mono text-[0.85em] text-ink">${token.text}</code>`;
			},
			hr() {
				return '<hr class="my-10 border-line" />\n';
			},
			list(token) {
				const tag = token.ordered ? 'ol' : 'ul';
				const style = token.ordered ? 'list-decimal' : 'list-disc';
				const start = token.ordered && token.start !== 1 ? ` start="${token.start}"` : '';
				const items = token.items.map((item) => this.listitem(item)).join('');
				return `<${tag}${start} class="my-4 space-y-1.5 pl-6 ${style} marker:text-ink-3">${items}</${tag}>\n`;
			},
			listitem(item) {
				return `<li class="leading-relaxed text-ink">${this.parser.parse(item.tokens)}</li>\n`;
			},
			table(token) {
				const head = token.header.map((cell) => this.tablecell(cell)).join('');
				const rows = token.rows
					.map((row) => `<tr class="border-t border-line">${row.map((cell) => this.tablecell(cell)).join('')}</tr>`)
					.join('');
				return `<div class="my-5 overflow-x-auto rounded-lg border border-line"><table class="w-full min-w-[32rem] text-left text-sm"><thead class="bg-surface-2"><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>\n`;
			},
			tablecell(token) {
				const tag = token.header ? 'th' : 'td';
				const align = token.align ? ` style="text-align:${token.align}"` : '';
				return `<${tag}${align} class="px-3 py-2 align-top text-ink ${token.header ? 'font-semibold' : ''}">${this.parser.parseInline(token.tokens)}</${tag}>`;
			},
			link(token) {
				const href = token.href;
				const external = /^https?:\/\//.test(href);
				const target = external ? ' target="_blank" rel="noopener noreferrer"' : '';
				const title = token.title ? ` title="${escape(token.title)}"` : '';
				return `<a href="${escape(href)}"${title}${target} class="${LINK_CLASS}">${this.parser.parseInline(token.tokens)}</a>`;
			}
		}
	});

	return { html: marked.parser(tokens), toc };
}
