import type { Me, Permission, Project } from '../api/client';

export function permissionsOn(me: Me, project: Project): Set<Permission> {
  const { installation, organisations, projects } = me.permissions;
  return new Set([
    ...installation,
    ...(organisations[project.organisation_id] ?? []),
    ...(projects[project.id] ?? []),
  ]);
}

export function permissionsOnOrganisation(me: Me, organisation: string): Set<Permission> {
  const { installation, organisations } = me.permissions;
  return new Set([...installation, ...(organisations[organisation] ?? [])]);
}

export function managesAgents(me: Me): boolean {
  return me.permissions.installation.includes('agent.manage');
}
