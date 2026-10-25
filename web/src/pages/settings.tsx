import { Code, Flex, Heading, Link, Text } from '@radix-ui/themes';
import { Link as RouterLink, useParams } from 'react-router';
import { ApiError, type Me, type Organisation, type Project } from '../api/client';
import { useOrganisation, useOrganisations, useProjectAt, useProjects } from '../api/queries';
import { useI18n } from '../i18n/i18n';
import { permissionsOn, permissionsOnOrganisation } from '../session/permissions';
import { useMe } from '../session/session';
import { Secrets, Variables } from '../settings/named-values';
import { Section } from '../shell/section';
import { Failure, Loading } from '../shell/status';

interface OrganisationViewProps {
  organisation: Organisation;
  me: Me;
}

interface ProjectViewProps {
  project: Project;
  me: Me;
}

interface ListProps {
  me: Me;
  projects: Project[];
}

interface ProjectListProps {
  projects: Project[];
}

interface MissingProps {
  message: string;
}

function viewable(me: Me, projects: Project[]): string[] {
  const ids = new Set<string>();
  for (const [id, keys] of Object.entries(me.permissions.organisations)) {
    if (keys.includes('project.view')) {
      ids.add(id);
    }
  }
  if (me.permissions.installation.includes('project.view')) {
    for (const project of projects) {
      ids.add(project.organisation_id);
    }
  }
  return [...ids];
}

function Missing({ message }: MissingProps) {
  const { t } = useI18n();
  return (
    <Flex direction="column" gap="3">
      <Heading as="h1" size="7">
        {t('notFound.title')}
      </Heading>
      <Text size="3">{message}</Text>
    </Flex>
  );
}

function OrganisationList({ me, projects }: ListProps) {
  const { t } = useI18n();
  const reads = useOrganisations(viewable(me, projects));
  const broken = reads.find((read) => read.isError);
  if (broken?.error) {
    return <Failure error={broken.error} onRetry={() => broken.refetch()} />;
  }
  if (reads.some((read) => read.isPending)) {
    return <Loading />;
  }
  const organisations = reads
    .flatMap((read) => (read.data === undefined ? [] : [read.data]))
    .sort((left, right) => left.slug.localeCompare(right.slug));
  if (organisations.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('settingsPage.organisations.empty')}
      </Text>
    );
  }
  return (
    <Flex asChild direction="column" gap="2" m="0" p="0">
      <ul className="plain-list">
        {organisations.map((organisation) => (
          <li key={organisation.id}>
            <Flex direction="column">
              <Link asChild size="2" weight="medium">
                <RouterLink to={`/settings/${organisation.slug}`}>{organisation.name}</RouterLink>
              </Link>
              <Code variant="ghost" size="1" color="gray">
                {organisation.slug}
              </Code>
            </Flex>
          </li>
        ))}
      </ul>
    </Flex>
  );
}

function ProjectList({ projects }: ProjectListProps) {
  const { t } = useI18n();
  if (projects.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t('home.projects.empty')}
      </Text>
    );
  }
  return (
    <Flex asChild direction="column" gap="2" m="0" p="0">
      <ul className="plain-list">
        {projects.map((project) => (
          <li key={project.id}>
            <Flex direction="column">
              <Link asChild size="2" weight="medium">
                <RouterLink to={`/projects/${project.path}/settings`}>{project.name}</RouterLink>
              </Link>
              <Code variant="ghost" size="1" color="gray">
                {project.path}
              </Code>
            </Flex>
          </li>
        ))}
      </ul>
    </Flex>
  );
}

export function SettingsPage() {
  const { t } = useI18n();
  const me = useMe();
  const projects = useProjects();
  return (
    <Flex direction="column" gap="5">
      <Heading as="h1" size="7">
        {t('shell.nav.settings')}
      </Heading>
      {projects.isPending ? <Loading /> : null}
      {projects.isError ? <Failure error={projects.error} onRetry={() => projects.refetch()} /> : null}
      {projects.data === undefined || !me.data ? null : (
        <>
          <Section title={t('settingsPage.organisations.title')}>
            <OrganisationList me={me.data} projects={projects.data} />
          </Section>
          <Section title={t('home.projects.title')}>
            <ProjectList projects={projects.data} />
          </Section>
        </>
      )}
    </Flex>
  );
}

function OrganisationView({ organisation, me }: OrganisationViewProps) {
  const { t } = useI18n();
  const allowed = permissionsOnOrganisation(me, organisation.id);
  const scope = { organisation: organisation.id };
  return (
    <Flex direction="column" gap="5">
      <Flex direction="column" gap="1">
        <Heading as="h1" size="7">
          {t('settingsPage.of', { name: organisation.name })}
        </Heading>
        <Code variant="ghost" size="2" color="gray">
          {organisation.slug}
        </Code>
      </Flex>
      <Section title={t('settingsPage.secrets.title')}>
        <Secrets scope={scope} writable={allowed.has('secret.manage')} />
      </Section>
      <Section title={t('settingsPage.variables.title')}>
        <Variables scope={scope} writable={allowed.has('variable.manage')} />
      </Section>
    </Flex>
  );
}

export function OrganisationSettingsPage() {
  const { t } = useI18n();
  const { organisation: slug = '' } = useParams();
  const me = useMe();
  const organisation = useOrganisation(slug);
  if (organisation.isPending || !me.data) {
    return <Loading />;
  }
  if (organisation.isError) {
    if (organisation.error instanceof ApiError && organisation.error.code === 'not_found') {
      return <Missing message={t('settingsPage.notFound', { name: slug })} />;
    }
    return <Failure error={organisation.error} onRetry={() => organisation.refetch()} />;
  }
  return <OrganisationView organisation={organisation.data} me={me.data} />;
}

function ProjectView({ project, me }: ProjectViewProps) {
  const { t } = useI18n();
  const allowed = permissionsOn(me, project);
  const scope = { project: project.id };
  const organisation = project.path.split('/')[0] ?? '';
  return (
    <Flex direction="column" gap="5">
      <Flex direction="column" gap="1">
        <Heading as="h1" size="7">
          {t('settingsPage.of', { name: project.name })}
        </Heading>
        <Link asChild size="2">
          <RouterLink to={`/projects/${project.path}`}>{project.path}</RouterLink>
        </Link>
      </Flex>
      <Text as="p" size="2">
        {t('settingsPage.inherited', { organisation })}
      </Text>
      <Section title={t('settingsPage.secrets.title')}>
        <Secrets scope={scope} writable={allowed.has('secret.manage')} />
      </Section>
      <Section title={t('settingsPage.variables.title')}>
        <Variables scope={scope} writable={allowed.has('variable.manage')} />
      </Section>
    </Flex>
  );
}

export function ProjectSettingsPage() {
  const { t } = useI18n();
  const { organisation = '', project: slug = '' } = useParams();
  const me = useMe();
  const project = useProjectAt(organisation, slug);
  if (project.isPending || !me.data) {
    return <Loading />;
  }
  if (project.isError) {
    if (project.error instanceof ApiError && project.error.code === 'not_found') {
      return <Missing message={t('projectPage.notFound', { path: `${organisation}/${slug}` })} />;
    }
    return <Failure error={project.error} onRetry={() => project.refetch()} />;
  }
  return <ProjectView project={project.data} me={me.data} />;
}
