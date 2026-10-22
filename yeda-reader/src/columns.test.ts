import assert from 'node:assert/strict';
import { test } from 'node:test';
import { regions } from './columns.ts';

const line = (x0: number, x1: number, y: number) => ({ x0, x1, y0: y, y1: y + 0.012 });
const lines = (x0: number, x1: number, from: number, to: number) =>
	Array.from({ length: Math.round((to - from) / 0.015) }, (_, i) => line(x0, x1, from + i * 0.015));
const round = (rs: ReturnType<typeof regions>) => rs.map(r => [r.x0, r.x1, r.y0, r.y1].map(n => Math.round(n * 100) / 100));

test('two columns, with a title and a full-width caption read across', () => {
	const page = [
		line(0.1, 0.9, 0.06), // running head
		line(0.2, 0.8, 0.1), line(0.3, 0.7, 0.12), // title, authors
		...lines(0.1, 0.48, 0.2, 0.5), ...lines(0.52, 0.9, 0.2, 0.45),
		line(0.1, 0.9, 0.7), // caption under a full-width figure
		...lines(0.1, 0.48, 0.75, 0.9), ...lines(0.52, 0.9, 0.75, 0.9),
		line(0.45, 0.55, 0.96), // page number
	];
	assert.deepEqual(round(regions(page)), [
		[0.09, 0.91, 0.04, 0.14],
		[0.09, 0.51, 0.19, 0.51], [0.49, 0.91, 0.19, 0.51],
		[0.09, 0.91, 0.49, 0.72],
		[0.09, 0.51, 0.74, 0.91], [0.49, 0.91, 0.74, 0.91],
	]);
});

test('a one-column page is one region', () => {
	const page = [...lines(0.1, 0.9, 0.1, 0.5), line(0.1, 0.3, 0.5), ...lines(0.1, 0.9, 0.52, 0.9)];
	assert.deepEqual(round(regions(page)), [[0.09, 0.91, 0.04, 0.9]]);
});
