import type { Me, Permission, Project } from '../api/client';

export function permissionsOn(me: Me, project: Project): Set<Permission> {
  const { installation, organisations, projects } = me.permissions;
  return new Set([
    ...installation,
    ...(organisations[project.organisation_id] ?? []),
    ...(projects[project.id] ?? []),
  ]);
}
