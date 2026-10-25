import { Flex, IconButton, Select } from '@radix-ui/themes';
import { Languages, Moon, Sun } from 'lucide-react';
import { isLanguage, LANGUAGE_NAMES, LANGUAGES, useI18n } from '../i18n/i18n';
import { useTheme } from '../theme/theme';

function LanguageSwitch() {
  const { language, setLanguage, t } = useI18n();
  return (
    <Select.Root
      value={language}
      onValueChange={(value) => {
        if (isLanguage(value)) {
          setLanguage(value);
        }
      }}
    >
      <Select.Trigger variant="ghost" aria-label={t('shell.language.label')}>
        <Flex as="span" align="center" gap="2">
          <Languages size={16} aria-hidden />
          {LANGUAGE_NAMES[language]}
        </Flex>
      </Select.Trigger>
      <Select.Content position="popper">
        {LANGUAGES.map((option) => (
          <Select.Item key={option} value={option} lang={option}>
            {LANGUAGE_NAMES[option]}
          </Select.Item>
        ))}
      </Select.Content>
    </Select.Root>
  );
}

function ThemeSwitch() {
  const { appearance, toggle } = useTheme();
  const { t } = useI18n();
  const label = appearance === 'dark' ? t('shell.theme.toLight') : t('shell.theme.toDark');
  return (
    <IconButton variant="ghost" color="gray" aria-label={label} title={label} onClick={toggle}>
      {appearance === 'dark' ? <Sun size={18} aria-hidden /> : <Moon size={18} aria-hidden />}
    </IconButton>
  );
}

export function Switches() {
  return (
    <Flex align="center" gap="4">
      <LanguageSwitch />
      <ThemeSwitch />
    </Flex>
  );
}
