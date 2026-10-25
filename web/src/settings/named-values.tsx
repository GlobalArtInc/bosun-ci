import { AlertDialog, Button, Code, Dialog, Flex, Heading, Table, Text } from '@radix-ui/themes';
import { type QueryKey, useMutation, useQueryClient } from '@tanstack/react-query';
import { Pencil, Plus, Trash2 } from 'lucide-react';
import { type FormEvent, type ReactNode, useId, useState } from 'react';
import type { NamedValue, Scope, Secret, Variable } from '../api/client';
import { useApi } from '../api/context';
import { secretsKey, useSecrets, useVariables, variablesKey } from '../api/queries';
import { type Key, useI18n } from '../i18n/i18n';
import { ago, stamp, useNow } from '../runs/time';
import { TextInput, textOf } from '../shell/inputs';
import { Refusal } from '../shell/refusal';
import { Failure, Loading } from '../shell/status';

interface Named {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
}

interface Words {
  empty: Key;
  setTitle: Key;
  set: Key;
  saved: Key;
  change: Key;
  changeTitle: Key;
  deleteQuestion: Key;
}

interface ScopedProps {
  scope: Scope;
  writable: boolean;
}

interface SetFormProps {
  words: Words;
  save: (request: NamedValue) => Promise<Named>;
  cacheKey: QueryKey;
}

interface ChangeProps {
  name: string;
  value?: string;
  words: Words;
  save: (request: NamedValue) => Promise<Named>;
  cacheKey: QueryKey;
}

interface RemoveProps {
  name: string;
  words: Words;
  remove: (name: string) => Promise<void>;
  cacheKey: QueryKey;
}

interface TableProps<T extends Named> {
  items: T[];
  words: Words;
  value?: (item: T) => string;
  actions?: (item: T) => ReactNode;
}

const VALUE_LIMIT = 49_152;

const SECRET_WORDS: Words = {
  empty: 'settingsPage.secrets.empty',
  setTitle: 'settingsPage.secrets.setTitle',
  set: 'settingsPage.secrets.set',
  saved: 'settingsPage.secrets.saved',
  change: 'settingsPage.secrets.replace',
  changeTitle: 'settingsPage.secrets.replaceTitle',
  deleteQuestion: 'settingsPage.secrets.deleteQuestion',
};

const VARIABLE_WORDS: Words = {
  empty: 'settingsPage.variables.empty',
  setTitle: 'settingsPage.variables.setTitle',
  set: 'settingsPage.variables.set',
  saved: 'settingsPage.variables.saved',
  change: 'settingsPage.variables.edit',
  changeTitle: 'settingsPage.variables.editTitle',
  deleteQuestion: 'settingsPage.variables.deleteQuestion',
};

function stored<T extends Named>(items: T[] | undefined, item: T): T[] | undefined {
  if (items === undefined) {
    return items;
  }
  return items.some((known) => known.id === item.id)
    ? items.map((known) => (known.id === item.id ? item : known))
    : [...items, item];
}

function useSave(save: (request: NamedValue) => Promise<Named>, cacheKey: QueryKey) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: save,
    gcTime: 0,
    onSuccess: (item) => {
      queryClient.setQueryData<Named[]>(cacheKey, (items) => stored(items, item));
    },
  });
}

function rawValue(form: FormData): string {
  return String(form.get('value') ?? '');
}

function SetForm({ words, save, cacheKey }: SetFormProps) {
  const { t } = useI18n();
  const heading = useId();
  const [generation, setGeneration] = useState(0);
  const set = useSave(save, cacheKey);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    set.mutate(
      { name: textOf(form, 'name'), value: rawValue(form) },
      { onSuccess: () => setGeneration((count) => count + 1) },
    );
  };

  return (
    <Flex direction="column" gap="3">
      <Heading as="h3" size="3" id={heading}>
        {t(words.setTitle)}
      </Heading>
      <form onSubmit={submit} aria-labelledby={heading} key={generation} className="form">
        <Flex direction="column" gap="3">
          <TextInput
            name="name"
            label={t('settingsPage.column.name')}
            hint={t('settingsPage.nameHint')}
            required
          />
          <TextInput
            name="value"
            label={t('settingsPage.column.value')}
            required
            multiline
            maxLength={VALUE_LIMIT}
          />
          <Flex>
            <Button type="submit" loading={set.isPending}>
              <Plus size={16} aria-hidden />
              {t(words.set)}
            </Button>
          </Flex>
        </Flex>
      </form>
      {set.isSuccess ? (
        <Text size="2" role="status">
          {t(words.saved, { name: set.data.name })}
        </Text>
      ) : null}
      {set.isError ? <Refusal error={set.error} /> : null}
    </Flex>
  );
}

function Change({ name, value, words, save, cacheKey }: ChangeProps) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const change = useSave(save, cacheKey);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    change.mutate({ name, value: rawValue(form) }, { onSuccess: () => setOpen(false) });
  };

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        change.reset();
      }}
    >
      <Dialog.Trigger>
        <Button size="1" variant="soft">
          <Pencil size={14} aria-hidden />
          {t(words.change)}
        </Button>
      </Dialog.Trigger>
      <Dialog.Content maxWidth="520px">
        <Dialog.Title>{t(words.changeTitle, { name })}</Dialog.Title>
        <form onSubmit={submit}>
          <Flex direction="column" gap="3">
            <TextInput
              name="value"
              label={t('settingsPage.column.value')}
              required
              multiline
              defaultValue={value}
              maxLength={VALUE_LIMIT}
            />
            {change.isError ? <Refusal error={change.error} /> : null}
            <Flex gap="3" justify="end">
              <Dialog.Close>
                <Button type="button" variant="soft" color="gray">
                  {t('form.cancel')}
                </Button>
              </Dialog.Close>
              <Button type="submit" loading={change.isPending}>
                {t('settingsPage.save')}
              </Button>
            </Flex>
          </Flex>
        </form>
      </Dialog.Content>
    </Dialog.Root>
  );
}

function Remove({ name, words, remove, cacheKey }: RemoveProps) {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const removal = useMutation({
    mutationFn: () => remove(name),
    onSuccess: () => {
      queryClient.setQueryData<Named[]>(cacheKey, (items) => items?.filter((known) => known.name !== name));
    },
  });
  return (
    <Flex direction="column" gap="2" align="start">
      <AlertDialog.Root>
        <AlertDialog.Trigger>
          <Button size="1" variant="soft" color="red" loading={removal.isPending}>
            <Trash2 size={14} aria-hidden />
            {t('settingsPage.delete')}
          </Button>
        </AlertDialog.Trigger>
        <AlertDialog.Content maxWidth="440px">
          <AlertDialog.Title>{t(words.deleteQuestion, { name })}</AlertDialog.Title>
          <AlertDialog.Description size="2">{t('settingsPage.deleteEffect')}</AlertDialog.Description>
          <Flex gap="3" mt="4" justify="end">
            <AlertDialog.Cancel>
              <Button variant="soft" color="gray">
                {t('settingsPage.keep')}
              </Button>
            </AlertDialog.Cancel>
            <AlertDialog.Action>
              <Button color="red" onClick={() => removal.mutate()}>
                {t('settingsPage.delete')}
              </Button>
            </AlertDialog.Action>
          </Flex>
        </AlertDialog.Content>
      </AlertDialog.Root>
      {removal.isError ? <Refusal error={removal.error} /> : null}
    </Flex>
  );
}

function NamedTable<T extends Named>({ items, words, value, actions }: TableProps<T>) {
  const { t, language } = useI18n();
  const now = useNow(false);
  if (items.length === 0) {
    return (
      <Text as="p" size="2" color="gray">
        {t(words.empty)}
      </Text>
    );
  }
  return (
    <Table.Root variant="surface" size="2">
      <Table.Header>
        <Table.Row>
          <Table.ColumnHeaderCell>{t('settingsPage.column.name')}</Table.ColumnHeaderCell>
          {value === undefined ? null : (
            <Table.ColumnHeaderCell>{t('settingsPage.column.value')}</Table.ColumnHeaderCell>
          )}
          <Table.ColumnHeaderCell>{t('runs.column.created')}</Table.ColumnHeaderCell>
          <Table.ColumnHeaderCell>{t('settingsPage.column.updated')}</Table.ColumnHeaderCell>
          {actions === undefined ? null : (
            <Table.ColumnHeaderCell>{t('form.actions')}</Table.ColumnHeaderCell>
          )}
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {items.map((item) => (
          <Table.Row key={item.id} align="center">
            <Table.RowHeaderCell>
              <Code variant="ghost" size="2" weight="medium">
                {item.name}
              </Code>
            </Table.RowHeaderCell>
            {value === undefined ? null : (
              <Table.Cell>
                <Code variant="soft" size="2" className="value">
                  {value(item)}
                </Code>
              </Table.Cell>
            )}
            <Table.Cell>
              <Text size="2" title={stamp(language, item.created_at)}>
                {ago(language, item.created_at, now)}
              </Text>
            </Table.Cell>
            <Table.Cell>
              <Text size="2" title={stamp(language, item.updated_at)}>
                {ago(language, item.updated_at, now)}
              </Text>
            </Table.Cell>
            {actions === undefined ? null : <Table.Cell>{actions(item)}</Table.Cell>}
          </Table.Row>
        ))}
      </Table.Body>
    </Table.Root>
  );
}

export function Secrets({ scope, writable }: ScopedProps) {
  const { t } = useI18n();
  const api = useApi();
  const secrets = useSecrets(scope);
  const cacheKey = secretsKey(scope);
  const save = (request: NamedValue) => api.setSecret(scope, request);
  const remove = (name: string) => api.deleteSecret(scope, name);
  return (
    <Flex direction="column" gap="4">
      <Text as="p" size="2">
        {t('settingsPage.secrets.explanation')}
      </Text>
      {secrets.isPending ? <Loading /> : null}
      {secrets.isError ? <Failure error={secrets.error} onRetry={() => secrets.refetch()} /> : null}
      {secrets.data === undefined ? null : (
        <NamedTable<Secret>
          items={secrets.data}
          words={SECRET_WORDS}
          actions={
            writable
              ? (secret) => (
                  <Flex gap="2" wrap="wrap">
                    <Change name={secret.name} words={SECRET_WORDS} save={save} cacheKey={cacheKey} />
                    <Remove name={secret.name} words={SECRET_WORDS} remove={remove} cacheKey={cacheKey} />
                  </Flex>
                )
              : undefined
          }
        />
      )}
      {writable ? <SetForm words={SECRET_WORDS} save={save} cacheKey={cacheKey} /> : null}
    </Flex>
  );
}

export function Variables({ scope, writable }: ScopedProps) {
  const { t } = useI18n();
  const api = useApi();
  const variables = useVariables(scope);
  const cacheKey = variablesKey(scope);
  const save = (request: NamedValue) => api.setVariable(scope, request);
  const remove = (name: string) => api.deleteVariable(scope, name);
  return (
    <Flex direction="column" gap="4">
      <Text as="p" size="2">
        {t('settingsPage.variables.explanation')}
      </Text>
      {variables.isPending ? <Loading /> : null}
      {variables.isError ? <Failure error={variables.error} onRetry={() => variables.refetch()} /> : null}
      {variables.data === undefined ? null : (
        <NamedTable<Variable>
          items={variables.data}
          words={VARIABLE_WORDS}
          value={(variable) => variable.value}
          actions={
            writable
              ? (variable) => (
                  <Flex gap="2" wrap="wrap">
                    <Change
                      name={variable.name}
                      value={variable.value}
                      words={VARIABLE_WORDS}
                      save={save}
                      cacheKey={cacheKey}
                    />
                    <Remove name={variable.name} words={VARIABLE_WORDS} remove={remove} cacheKey={cacheKey} />
                  </Flex>
                )
              : undefined
          }
        />
      )}
      {writable ? <SetForm words={VARIABLE_WORDS} save={save} cacheKey={cacheKey} /> : null}
    </Flex>
  );
}
