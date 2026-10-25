import type { Actor } from '../api/client';

const BRANCH = 'refs/heads/';

const TAG = 'refs/tags/';

export function actorName(actor: Actor): string {
  switch (actor.kind) {
    case 'agent':
      return actor.name;
    case 'system':
      return 'Bosun';
    default:
      return actor.login;
  }
}

export function refName(ref: string | null): string | null {
  if (ref === null) {
    return null;
  }
  if (ref.startsWith(BRANCH)) {
    return ref.slice(BRANCH.length);
  }
  return ref.startsWith(TAG) ? ref.slice(TAG.length) : ref;
}

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}
