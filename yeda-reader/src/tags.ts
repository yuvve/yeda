export interface Tag {
	name: string;
	color: string;
}

interface TagReader {
	_state: { annotations: { id: string; tags?: Tag[]; readOnly?: boolean }[]; readOnly: boolean };
	_annotationManager: { updateAnnotations(changes: { id: string; tags: Tag[] }[]): void };
	focusView(): void;
}

let open = false;

export function showTagPrompt(reader: TagReader, id: string) {
	const annotation = reader._state.annotations.find(a => a.id === id);
	if (open || reader._state.readOnly || !annotation || annotation.readOnly) return;
	open = true;

	const tags = new Map(annotation.tags?.map(tag => [tag.name, tag]));
	const names = [...new Set(reader._state.annotations.flatMap(a => a.tags?.map(t => t.name) ?? []))].sort();
	const overlay = document.createElement('div');
	overlay.style.cssText = 'position:fixed;inset:0;z-index:10000;display:flex;justify-content:center;align-items:flex-start;padding-top:10vh;background:rgba(0,0,0,.3)';
	const box = document.createElement('div');
	box.style.cssText = 'width:min(640px,90vw);display:flex;flex-direction:column;background:Canvas;color:CanvasText;border:1px solid GrayText;border-radius:6px;font:14px sans-serif;box-shadow:0 8px 32px rgba(0,0,0,.4)';
	const input = document.createElement('input');
	input.placeholder = 'Tags, separated by commas';
	input.setAttribute('aria-label', 'Annotation tags');
	input.style.cssText = 'padding:10px;border:0;border-bottom:1px solid GrayText;background:transparent;color:inherit;font:inherit;outline:none';
	input.value = [...tags.keys()].join(', ');
	const hint = document.createElement('div');
	hint.style.cssText = 'padding:10px;color:GrayText;overflow-wrap:anywhere;max-height:40vh;overflow:auto';
	hint.textContent = `Tags in this PDF: ${names.join(', ') || 'none'}`;
	box.append(input, hint);
	overlay.append(box);

	function close(focus: boolean) {
		if (!open) return;
		open = false;
		overlay.remove();
		if (focus) reader.focusView();
	}
	input.addEventListener('keydown', event => {
		if (event.key !== 'Enter' && event.key !== 'Escape') return;
		event.preventDefault();
		event.stopPropagation();
		if (event.key === 'Enter') {
			const chosen = new Set(input.value.split(',').map(name => name.trim()).filter(Boolean));
			reader._annotationManager.updateAnnotations([{ id, tags: [...chosen].map(name => tags.get(name) ?? { name, color: '' }) }]);
		}
		close(true);
	});
	input.addEventListener('blur', () => close(false));
	document.body.append(overlay);
	input.focus();
	input.select();
}
