import { Box, Container, Flex, Separator, Text } from '@radix-ui/themes';
import { Anchor } from 'lucide-react';
import { Link, Navigate, NavLink, Outlet, useLocation } from 'react-router';
import { useI18n } from '../i18n/i18n';
import { LiveProvider } from '../live/live';
import { here, passwordPath, signInPath, useMe } from '../session/session';
import { Failure, Loading } from './status';
import { Switches } from './switches';
import { UserMenu } from './user-menu';

export function Wordmark() {
  return (
    <Flex align="center" gap="2" asChild>
      <Link to="/" className="wordmark">
        <Anchor size={20} aria-hidden />
        <Text size="4" weight="bold">
          Bosun
        </Text>
      </Link>
    </Flex>
  );
}

export function Shell() {
  const { t } = useI18n();
  const location = useLocation();
  const me = useMe();

  if (me.isPending) {
    return <Loading />;
  }
  if (me.isError) {
    return (
      <Container size="2" p="6">
        <Failure error={me.error} onRetry={() => me.refetch()} />
      </Container>
    );
  }
  if (me.data === null) {
    return <Navigate to={signInPath(here(location))} replace />;
  }
  if (me.data.password_change_required) {
    return <Navigate to={passwordPath(here(location))} replace />;
  }
  return (
    <Flex direction="column" minHeight="100vh">
      <Box asChild px="5" py="3">
        <header>
          <Flex align="center" justify="between" gap="3" wrap="wrap">
            <Flex align="center" gap="6">
              <Wordmark />
              <nav aria-label={t('shell.nav.label')}>
                <Flex asChild align="center" gap="4" m="0" p="0">
                  <ul className="navigation">
                    <li>
                      <NavLink to="/" end className="navigation-link">
                        {t('shell.nav.home')}
                      </NavLink>
                    </li>
                  </ul>
                </Flex>
              </nav>
            </Flex>
            <Flex align="center" gap="4">
              <Switches />
              <UserMenu me={me.data} />
            </Flex>
          </Flex>
        </header>
      </Box>
      <Separator size="4" />
      <Box asChild px="5" py="5" flexGrow="1">
        <main>
          <LiveProvider>
            <Outlet />
          </LiveProvider>
        </main>
      </Box>
    </Flex>
  );
}
