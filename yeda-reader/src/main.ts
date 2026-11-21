import { exit } from '@tauri-apps/api/app';
import { openReader } from './reader';

await openReader(() => void exit(0));
