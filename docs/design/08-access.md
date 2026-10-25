# Access

This document specifies who can do what: users and how they sign in, sessions and tokens,
organisations, projects and teams, permissions and roles, the trust model for code from outside
the team, the rules for drafts and terminals, and the audit trail.

## Users and sign-in

A user has a login, a display name, an email address and any number of linked identities. A login
is 1 to 64 lowercase letters, digits, `-`, `_` or `.`, starts with a letter or a digit, and never
reads as an id (a UUID), because a request names a user by its id or its login.

- **Local password**, hashed with argon2id. Local accounts can add TOTP as a second factor
  (from M3).
- **OpenID Connect**, with one or more providers in the server configuration (issuer, client,
  claim names for login, email and groups). A first sign-in creates the user when the provider's
  claims match the configured allow rules (groups or email domains); otherwise it is refused
  ([OpenID Connect](#openid-connect)).
- **Forge account link.** A user links their forge account through the App's user authorisation.
  The link maps forge actors to Bosun users: for re-runs requested on the forge, for approvals
  shown on the forge, and for `github.actor` of runs started in Bosun.

The first owner is created by `bosun-server admin create <login>`, or from `BOSUN_BOOTSTRAP_ADMIN`
on the first start against a database without users; either way the server prints one personal token
for the owner, so the CLI works before any sign-in method is configured.

- The owner has its login as its display name and no email address. Its token is named
  `bootstrap` and expires one year (365 days) after it was created.
- `admin create` brings the schema up to date first, refuses a login that already exists, and
  writes the token, and nothing else, to standard output; its log lines go to standard error and
  never contain the token. It may create further owners.
- `BOSUN_BOOTSTRAP_ADMIN` is the setting `bootstrap.admin` ([12-operations.md](12-operations.md)).
  `serve` acts on it after the migrations and before it listens, and only when the database holds
  no user at all, so the setting can stay in a deployment without creating anyone on later starts.
  The token is then a field of the log line `created the owner from bootstrap.admin; its token is
  shown only this once`, since the log is all `serve` prints. Replicas that start at once decide
  under an advisory lock: one creates the owner, the others log that the database has users
  already.
- Without `bootstrap.admin`, `serve` on a start against a database without users creates the
  owner `admin` the same way and writes its token to `admin.token` in the data directory, readable
  only by the server's user, instead of logging it. This is how a fresh installation on any host
  is entered: `podman exec bosun cat /bosun-data/admin.token`. The file holds the token and a
  newline, has mode `0600` and is renamed into place; a start against a database with users
  leaves it alone. When the file cannot be written after the owner was created, `serve` exits
  with an error that says so, and `admin create` makes another owner. From M1, when local
  passwords exist,
  the server also writes `admin.password`, and the first sign-in with it asks for a new password
  and deletes both files ([Local sign-in](#local-sign-in)).

Failed sign-ins are rate-limited per account and per address, with an exponentially growing
lockout.

### Local sign-in

Step 8 of M1 ([../plan.md](../plan.md)) settled the mechanisms below; the endpoints are in
[11-api.md](11-api.md#sign-in-and-sessions).

- **Passwords.** A password is 12 to 1024 characters. `user.password_hash` holds its argon2id hash
  as a PHC string, with the parameters the OWASP guidance names as its minimum (19 MiB of memory,
  2 passes, 1 lane, a random salt of 16 bytes). A check reads the parameters from the stored string,
  so raising them later needs no migration. Hashing runs on blocking threads, at most one per CPU at
  once, so a burst of sign-ins takes neither the memory of the server nor the threads that answer
  requests. A user without a password, such as an owner made by `admin create`, cannot sign in with
  one.
- **Sign-in.** `POST /api/v1/auth/login` takes a login and a password. A wrong password, an unknown
  login and a user without a password get the same `401` after the same work, since an unknown
  login is checked against a decoy hash, so neither the answer nor its time tells whether a login
  exists. A request that checks a password must have the content type `application/json`, which a
  form of another site cannot send without a preflight the server never grants, so another site
  cannot sign a browser in to an account of its choosing.
- **Sessions.** A sign-in begins a session: a `bss_` credential in the format below, stored as its
  SHA-256 in `session` with its user, the time it began and the time of its last request. The
  answer sets `__Host-bosun_session=bss_…; Max-Age=2592000; Path=/; Secure; HttpOnly;
  SameSite=Lax`. A session ends 12 hours after its last request and 30 days after it began,
  whichever comes first; the time of the last request is written at most once a minute, so the idle
  timeout holds to the minute. `POST /api/v1/auth/logout` deletes the session at once and empties
  the cookie. Every minute a periodic task removes ended sessions
  ([07-storage.md](07-storage.md#periodic-tasks)). A request with an `Authorization` header is
  authenticated by that header alone and the cookie is not read.
- **CSRF.** The CSRF token of a session is the SHA-256 of `bosun csrf`, a zero byte and the `bss_`
  text, written as 43 base62 digits. It is bound to its session without being stored, and it does
  not reveal the session. `GET /api/v1/me` and the sign-in return it as `csrf_token`, which is
  `null` for a bearer token. A request that the cookie authenticates and whose method is not `GET`,
  `HEAD` or `OPTIONS` must carry it in `X-Bosun-CSRF`; without the header, or with the token of
  another session, it is `403 forbidden` before its handler runs. The comparison takes the same
  time whatever the header holds.
- **Lockout.** Every sign-in counts against the account, when the given login is a login at all,
  and against the address it comes from, each a row of `sign_in_failure`. An attempt is counted as
  a failure before its password is checked, in a transaction that locks both rows, so concurrent
  attempts never get more tries than the limit. An attempt that succeeds then deletes the count of
  its account and takes its own failure back from the count of its address, so sign-ins that
  succeed never lock an address. After `limits.failed_sign_ins` failures (10 by default,
  [12-operations.md](12-operations.md#configuration)) the account and the address are locked for a
  minute, and each failure after a lock ended doubles the period, up to a day. During a lock a
  sign-in is `429 rate_limited` with `Retry-After` in seconds and a message that names the account
  or the address, and its password is not checked. A count is forgotten a day after its last
  failure.
- **The address.** The address of a request is the peer of its connection. When that peer is in
  `server.trusted_proxies`, the server reads `X-Forwarded-For` from the right for as long as the
  hop it has reached is a trusted proxy, and the first hop that is not one is the address; a hop
  that is not an address ends the walk at the last trusted proxy. From any other peer the header is
  ignored, so a client cannot choose the address it is counted under. An IPv4 address mapped into
  IPv6 counts as the IPv4 address.
- **Changing one's own password.** `POST /api/v1/auth/password` takes `current_password` and
  `new_password`, from a session with its CSRF token or with a personal token. The current password
  is required every time, so a stolen session or token cannot change the password and lock its
  owner out, and a wrong one counts like a failed sign-in. The new password follows the rule above
  and differs from the current one. The change ends every other session of the user, keeps the one
  that made it, and clears `user.password_change_required`. A user without a password gets `409
  invalid_state`: there is no password to change.
- **A password that must change.** While `user.password_change_required` is set, a session of that
  user may call only `GET /api/v1/me`, `POST /api/v1/auth/logout` and `POST /api/v1/auth/password`;
  any other request is `403 forbidden` saying that a new password comes first. Personal tokens are
  not restricted, so `admin.token` signs the CLI in before anyone sets a password.
- **The first start.** The owner `admin` that the first start creates has a generated password of
  43 base62 digits, and must change it. `admin.password` holds it next to `admin.token`, both
  readable only by the server's user, in a data directory the server creates with mode `0700` when
  it is missing. When `admin` changes that password, the server deletes both files from its data
  directory; the token stays valid until it expires or is revoked. A replica whose data directory
  does not hold the files deletes nothing.

### OpenID Connect

Step 11 of M1 ([../plan.md](../plan.md)) settled the mechanisms below; the providers are
configured as `[[oidc]]` ([12-operations.md](12-operations.md#configuration)) and the endpoints are
in [11-api.md](11-api.md#sign-in-and-sessions).

- **Discovery.** A provider's configuration is the discovery document at
  `<issuer>/.well-known/openid-configuration`, read when a sign-in first needs it and kept for an
  hour; its `issuer` must be the configured issuer exactly, or the provider is not used. The keys
  of `jwks_uri` are read when an ID token first needs them and read again when a token names a
  key that the kept ones lack, so a provider that rotates its keys needs no restart. A provider
  that does not answer fails the sign-in with `500 internal`, whose cause the server's log has;
  the server starts without asking any provider, so a provider that is down stops only its own
  sign-ins.
- **The start.** `GET /api/v1/auth/oidc/{provider}/start` makes a state, a nonce and a PKCE
  verifier, each 32 random bytes in base64url, and sends the browser to the authorization
  endpoint with `response_type=code`, the client, the scopes, the state, the nonce, and the
  challenge of the verifier with the method `S256`. The address the provider returns the browser
  to is `<server.public_url>/api/v1/auth/oidc/<provider>/callback`, which the client at the
  provider must allow.
- **The cookie of the sign-in.** The state, the nonce, the verifier, the provider and the time
  the sign-in ends, 10 minutes after it starts, are a JSON object in the cookie
  `__Host-bosun_oidc` (`Max-Age=600; Path=/; Secure; HttpOnly; SameSite=Lax`), written as its
  base64url, a dot and the base64url of the HMAC-SHA256 of `bosun oidc state`, a zero byte and
  that base64url. The key of the HMAC is derived from the KEK, as an HMAC-SHA256 of
  `bosun signing key` and a zero byte under the KEK, so every replica that shares the KEK checks
  the cookie of any other, and nobody without the KEK makes or changes one. The cookie is not
  encrypted: it holds nothing that the browser which started the sign-in may not know. A cookie signed under an earlier KEK that `keys.previous_kek_files` still
  names is taken too, so a rotation does not fail the sign-ins under way. `SameSite=Lax` lets the
  browser send the cookie with the provider's redirect, which is a top-level navigation.
- **The callback.** `GET /api/v1/auth/oidc/{provider}/callback` refuses a callback whose browser
  holds no cookie of a sign-in through that provider, whose cookie Bosun did not sign, whose
  sign-in started more than 10 minutes ago, or whose `state` is not the state of the cookie: a
  callback that belongs to another browser never signs this one in. It then records the state as
  used, as its SHA-256 in `oidc_state` until the sign-in would have ended, and refuses a state
  that was used already, so a callback works once even when its cookie and its address were
  copied. Only after that does it look at the provider's `error`, at an `iss` that is not the
  provider's issuer ([RFC 9207](https://www.rfc-editor.org/rfc/rfc9207)), and at the code, which
  it exchanges at the token endpoint with the verifier, authenticating the client with
  `client_secret_basic`.
- **The ID token.** The token is a JWS signed with RS256, the algorithm every provider offers
  and the only one taken, so neither `none` nor an HMAC under a published key passes. Its key is
  the RSA signing key of the provider's set whose `kid` the token names, or the only one when the
  token names none. The claims must hold the configured issuer as `iss`, the client among `aud`,
  and as `azp` when there are several audiences or `azp` is present, an `exp` after the time of
  the callback, the nonce of the cookie, and a `sub`. The person's claims are read from the ID
  token alone, so the provider puts the login, the email address and the groups into it.
- **Users and identities.** `user_identity` links the provider's `iss` and `sub` to a user, which
  OpenID Connect names as the only stable identifier of a person; the name of the provider in the
  configuration is not part of it, so renaming a provider keeps its users. A sign-in whose
  identity is linked signs in its user. A first sign-in creates a user from the claims: the login
  claim in lower case, which must be a login, the `name` claim as the display name or the login
  when it has none, and the email claim; the user has no password and is not an owner. A login
  that another user holds refuses the first sign-in with `409 conflict`, since a provider could
  name any login and an identity is never linked to an existing user by its login. Deleting the
  user deletes its identities, so its next sign-in creates a new user.
- **Allow rules.** A person is admitted when one of their groups is in `allowed_groups`, or when
  their email address is marked verified (`email_verified: true`) and its domain, compared
  without regard to case, is in `allowed_email_domains`; an address the provider has not verified
  could be anyone's. A provider without rules admits nobody. The rules are checked at every
  sign-in, not only the first, so a person whom the provider no longer puts in an allowed group
  cannot sign in again; their user and their other sessions stay until an owner deletes the
  user.
- **Teams from groups.** `[[oidc.teams]]` maps a group to a team of an organisation. At every
  sign-in, for each team the provider maps, the person becomes a member when one of their groups
  maps to it and stops being one otherwise, so leaving the group at the provider leaves the team
  at the next sign-in, and with the team its bindings
  ([When it is read](#roles-and-bindings)). A mapped team that does not exist yet is created at
  the first sign-in that makes someone a member, with its slug as its name: the mapping states
  that the team should exist, and a team grants nothing until it has a binding. A mapping of an
  organisation that does not exist is skipped with a warning in the log, since creating an
  organisation is an owner's decision. Teams of an organisation that no mapping of the provider
  names, and the members that others added to them, are left alone.
- **The session** is a session of [Local sign-in](#local-sign-in) in every respect. A successful
  callback sets its cookie, empties `__Host-bosun_oidc` and sends the browser to the web interface
  at `server.public_url`.
- **Removal.** The periodic task that removes ended sessions also removes the rows of
  `oidc_state` whose sign-in has ended.

## Sessions and tokens

| Credential | Form | Notes |
|------------|------|-------|
| browser session | cookie `__Host-bosun_session` holding `bss_…` | `Secure`, `HttpOnly`, `SameSite=Lax`; idle timeout 12 hours, absolute 30 days |
| personal token | `bst_…` | named; a set of permission keys; optionally restricted to organisations or projects; expiry required, at most one year |
| agent registration token | `brt_…` | for one pool; single or repeated use; expiry, by default 24 hours and at most one year |
| agent credential | `bat_…` | one per agent; a bearer token in the `authorization` metadata of the agent's stream |
| plugin credential | `bpt_…` | one per external plugin instance |
| job token | JWT | signed by the server, for the runtime services only ([06-runtime-services.md](06-runtime-services.md)) |

Opaque credentials are a prefix, 32 random bytes in base62 and a CRC32 checksum, so secret scanners
can recognise them; the server stores only their SHA-256. The 32 bytes are a big-endian number
written as exactly 43 base62 digits (`0`–`9`, `A`–`Z`, `a`–`z` in that order, padded with `0`),
followed by the CRC-32 (ISO-HDLC, the checksum of zlib) of the prefix and those 43 digits, written
as exactly 6 base62 digits, so a credential with a four-character prefix such as `bst_` is 53
characters long. The SHA-256 is that of the whole text. The server checks the prefix, the length,
the digits and the checksum before it looks a credential up, so a mistyped, truncated or relabelled
credential never costs a query; a malformed session cookie is refused the same way. A mutating
request authenticated by the cookie must carry `X-Bosun-CSRF` with the token bound to the session,
which `GET /api/v1/me` returns; bearer tokens do not need it ([Local sign-in](#local-sign-in)).

## Organisations, projects and teams

An **organisation** has a slug, a display name and optionally a forge installation
([06-runtime-services.md](06-runtime-services.md#installations-and-repositories) says how an
installation maps to one). A **project**
belongs to one organisation, has a slug and optionally a repository, and carries the project
settings this design names elsewhere (`github_workflows`, `workspace`, the fork policy, default
token permissions, checks or statuses, dispatch weight and running cap, retention). A **team** is a
set of users inside an organisation, maintained by hand or synchronised from a groups claim of an
identity provider through a configured mapping.

A slug follows the rule of logins: 1 to 64 lowercase letters, digits, `-`, `_` or `.`, starting
with a letter or a digit. An organisation's slug is unique in the installation and never reads as an
id (a UUID), because a path names an organisation by either; a project's slug is unique in its
organisation. A display name is 1 to 255 characters without control characters, not all of them
white space.

## Permissions and roles

| Permission | Allows |
|------------|--------|
| `project.view` | see the project, its workflows, runs, logs, artifacts |
| `run.dispatch` | start runs by hand or through the API |
| `run.cancel` | cancel runs and jobs |
| `run.rerun` | re-run runs and jobs |
| `run.control` | pause, resume, skip, breakpoints, priority, abort a step |
| `run.override` | change a result: mark jobs succeeded, continue past a held step, abort a step as skipped; admit runs from forks |
| `job.debug` | open terminals in held jobs, retry or release them |
| `workflow.edit` | edit drafts and server-side workflows, run drafts |
| `workflow.commit` | save a draft to the repository |
| `workflow.manage` | enable and disable workflows |
| `secret.manage`, `variable.manage` | write secrets and variables |
| `environment.manage` | define environments and their rules |
| `environment.bypass` | admit a job to an environment without its approvals, where the environment allows it |
| `agent.manage` | pools, agents, registration tokens, pinning jobs |
| `plugin.manage` | install plugins, configure instances |
| `project.manage` | project settings, create and delete projects |
| `organisation.manage` | members, teams, roles, organisation settings |
| `audit.view` | read the audit trail |

| Role | Permissions |
|------|-------------|
| `viewer` | `project.view` |
| `developer` | viewer, `run.dispatch`, `run.cancel`, `run.rerun`, `run.control` |
| `maintainer` | developer, `run.override`, `job.debug`, `workflow.*`, `secret.manage`, `variable.manage`, `environment.manage` |
| `admin` | maintainer, `environment.bypass`, `agent.manage`, `plugin.manage`, `project.manage`, `organisation.manage`, `audit.view` |

An organisation can define its own roles as any set of permission keys. A role binding gives a
user or a team a role on an organisation or on one project; a person's permissions on a project are
the union of their bindings on the project and on its organisation. Installation owners hold every
permission everywhere.

Approving a gate or an environment is not a permission: the approvers are named in the gate or in
the environment's rules. An approver must also hold `project.view`.

### Roles and bindings

Step 9 of M1 ([../plan.md](../plan.md)) settled how roles and bindings are kept and read; how the
API answers is in [11-api.md](11-api.md#permissions).

- **Roles** are rows of `role`, each a name and a set of permission keys. The four built-in roles
  belong to no organisation; the migrations create them with the sets of the table above, with
  `workflow.*` standing for the three `workflow.` keys. A custom role belongs to one organisation
  and holds any set of keys. A key that the server does not know, which a newer minor version may
  have written, is ignored.
- **Teams** are rows of `team` in one organisation, with a slug and a name like a project's, and
  their members are rows of `team_member`.
- **Bindings** are rows of `role_binding`: a role, either a user or a team, the organisation the
  binding belongs to, and either no project, for a binding on the organisation, or one of its
  projects. Foreign keys on the pairs of project and organisation and of team and organisation keep
  a binding's project and team inside its organisation; a binding of a custom role of another
  organisation, which no foreign key can refuse, gives nothing.
- **What a person holds.** On the installation, an owner holds every permission and nobody else
  holds any. On an organisation, a person holds what they hold on the installation and what the
  bindings on that organisation give them, their own and those of the teams they are members of.
  On a project, they hold what they hold on its organisation and what the bindings on that project
  give them. A binding on a project gives nothing on its organisation or on its other projects.
- **When it is read.** The server reads the bindings of a person who is not an owner, with one
  query, on every request their credentials authenticate, and keeps nothing between requests: a
  binding removed, or a member removed from a team, is gone from their next request.
- **Tokens.** A personal token acts with the permissions of its user, restricted to the keys and
  the places it carries ([Users, teams, roles and tokens](#users-teams-roles-and-tokens)).
- **The installation.** Bindings exist only on organisations and projects, so what is checked on
  the installation is held by owners alone: creating organisations (`organisation.manage`), and the
  pools, agents and registration tokens, which belong to the installation (`agent.manage`). So do
  the label aliases, an organisation's too (step 18 of [../plan.md](../plan.md)): an alias chooses
  the pool that receives a project's jobs, and with them its secrets, and pools are the secret
  boundary the owners draw ([What a stolen credential gets](#what-a-stolen-credential-gets)).

### Users, teams, roles and tokens

Step 10 of M1 ([../plan.md](../plan.md)) settled who manages these and how a token's permissions
are computed; the endpoints are in [11-api.md](11-api.md#identity-and-access).

- **Installation owners manage users.** Only an owner creates, lists and deletes users, since a user
  belongs to the installation rather than to an organisation. A local user is created with a login,
  a display name, an email address and an initial password, and must replace that password at the
  first sign-in, through the change of password of [Local sign-in](#local-sign-in); until then its
  sessions reach nothing else. Deleting a user removes its sessions, tokens, team memberships,
  bindings and linked identities with it. An owner cannot delete themselves, so an installation always keeps an owner.
- **Organisations manage their teams, roles and bindings.** Teams with their members, custom roles
  and role bindings are each managed with `organisation.manage` in the organisation they belong
  to; a binding on a project belongs to the project's organisation. `organisation.manage` lets its
  holder bind anyone, themselves included, to any role in the organisation, so it is as strong as
  every permission there, and a custom role that holds it holds them all in effect.
- **Names.** The built-in roles are fixed: they are neither changed nor deleted, and no custom role
  takes the name of one. A custom role's name follows the rule of slugs and is unique in its
  organisation; neither it nor a team's slug reads as an id, so a binding names its role, its user
  and its team by id or by name without ambiguity.
- **What a personal token carries.** A token carries a set of permission keys, every one of which
  its creator holds where the token reaches, and may be restricted to organisations and projects
  the creator holds something on. It expires within a year (365 days) of its creation. Its owner,
  the user who created it, lists and revokes it; a revoked token stays in the list with the time it
  was revoked. The tokens the server makes for an owner it creates (`bootstrap` and `admin.token`)
  carry every key and no restriction. The keys are the column `api_token.permissions`, `NULL` for
  those tokens of the server, which so carry the keys of later versions too; a restriction is
  `api_token.restricted` with one row of `api_token_scope` per organisation or project, which the
  deletion of that organisation or project removes.
- **What a personal token holds.** A token acts with the intersection of its keys and its creator's
  permissions at the time of each request, read as the creator's bindings are
  ([When it is read](#roles-and-bindings)), so a creator who loses a permission takes it from their
  tokens as well, and one who gains a permission the token carries gives it to them. An
  unrestricted token holds that intersection everywhere. A token restricted to organisations and
  projects holds nothing on the installation, even when its creator is an owner; on each of its
  organisations, and on the projects of that organisation, it holds what its creator holds there
  within its keys; on each of its projects, what its creator holds on that project, the bindings
  of its organisation included, within its keys. A restricted token whose organisations and
  projects were all deleted stays restricted and reaches nothing.
- **Tokens made with a token.** A token may create tokens, as `bosun token create` does with the
  token of its profile, but only within its own reach: the keys the new token carries must be held
  by the token that creates it, a restricted token makes only tokens restricted to its own
  organisations and projects or some of their projects, and the new token expires no later than
  the one that made it. A token made with a stolen one therefore reaches no further and lives no
  longer than the stolen token would have. Revoking a token does not revoke the tokens it made, so
  that rotating the token of a profile keeps the tokens made with it; after a theft their owner
  revokes those too, from the list of their tokens.

### Secrets and variables

Step 24 of M1 ([../plan.md](../plan.md)) settled who reads and writes them; the endpoints are in
[11-api.md](11-api.md#secrets-and-variables) and their storage in
[07-storage.md](07-storage.md#secrets-at-rest).

- **Scope.** A secret or a variable belongs to an organisation, and reaches every project of it, or
  to one project. A project's secret or variable shadows the organisation's of the same name, which
  the project's jobs then do not receive. Environment secrets arrive with environments in M3.
- **Writing** a secret needs `secret.manage`, and a variable `variable.manage`, on its organisation
  or its project, so a maintainer writes them and a developer does not. A binding on a project gives
  nothing on its organisation, so writing an organisation's secrets needs a binding on the
  organisation.
- **Viewers see the names of secrets.** Listing the secrets of an organisation or a project needs
  only `project.view` there, and shows names and times, never values, which no answer holds. The
  names are not secret: the workflows that every viewer reads name them in `secrets.NAME`, and a
  developer who cannot write a secret still needs to know which ones a project has. The values
  reach only the jobs that read them.
- **Variables are not secret.** Anyone with `project.view` reads their values, as the logs that
  print them show them anyway, and they reach untrusted runs ([below](#code-from-outside-the-team)).

## Code from outside the team

Every pool has a **trust level**.

- `trusted` pools may have agents with the `shell` capability, give jobs the podman socket, mount
  cache volumes and allow privileged container options.
- `untrusted` pools have `podman` agents only, without the socket, without cache volumes and
  without privileged options, and keep nothing between jobs ([05-execution.md](05-execution.md)).
  Their agents should live in a network with no access to anything internal; Bosun cannot check
  that and the pool page says so.

The claim query ([07-storage.md](07-storage.md)) enforces both directions, whatever the labels say:
a job of an untrusted run never reaches a trusted pool, and a job of a trusted run, which carries
secrets, never reaches an untrusted pool.

A run is untrusted when it was triggered by a pull request from a fork. The project's fork policy,
its setting `fork_policy` ([11-api.md](11-api.md#project-settings)), decides what happens to such a
pull request:

| Policy | Effect |
|--------|--------|
| `disabled` (default) | no run is created; the pull request gets a status saying so |
| `approval` | the run is created and its jobs wait with reason `untrusted` until a maintainer admits it (`run.admit`), from the run page or with `bosun run admit` |
| `automatic` | the run is created at once |

An untrusted run receives no secrets and no environment secrets; variables are passed, since they
are not meant to be secret. Its forge token is read-only. `pull_request_target` runs are trusted, because they run the base
branch's workflow, and the validator warns when such a workflow checks out the pull request's head.

Step 29 of M1 ([../plan.md](../plan.md)) settled how the policy applies:

- **From a fork.** A pull request is from a fork when the repository of its head
  (`pull_request.head.repo.id`) is not the repository of its base, or when its head names no
  repository, as when the fork was deleted. The policy applies to the events that run the pull
  request's own code at its merge ref: `pull_request`, `pull_request_review` and
  `pull_request_review_comment`. `pull_request_target` runs the base branch's files and
  `merge_group` the queue's commit in the repository itself, so their runs are trusted whoever
  opened the pull request.
- **`disabled`** creates no run, not even a `startup_failure`, and reads nothing of the fork: the
  server reads the workflows of the base branch's last commit, the files the project has reviewed,
  and matches the event against them with the pull request's files. When one of them would run,
  the pull request gets a commit status on its head commit through the outbox
  ([07-storage.md](07-storage.md#the-outbox)): the context `bosun/fork-policy`, the state `error`,
  since nothing ran rather than something failed, the description `Bosun runs no workflow for pull
  requests from forks in this project`, and no target address. A fork that adds a workflow file
  of its own therefore gets no status for it, since that file never runs. An event that no
  workflow of the base branch matches, such as a label added under the default `types`, posts
  nothing, and one that matches again posts the same status again, which the forge keeps once per
  context and commit.
- **`approval`** creates the run untrusted, `run.trusted` false and `run.admitted_at` empty. Each
  job that becomes ready waits with reason `untrusted`, and the run is `waiting`
  ([04-runs.md](04-runs.md#states)). `run.admit` needs `run.override`, sets `admitted_at`, and
  queues the waiting jobs with the command's actor; jobs that become ready afterwards are queued as
  in any run. Until then the run is a member of no concurrency group
  ([04-runs.md](04-runs.md#concurrency-groups)), and it enters its workflow's group when it is
  admitted. The interface shows the command on the run page from step 42.
- **`automatic`** creates the run untrusted and admitted at once, so its jobs are queued as they
  become ready.
- **The untrusted run** goes to untrusted pools only
  ([05-execution.md](05-execution.md#pools-and-label-aliases)). Its assignment holds no secret of
  the organisation or the project, whatever the job references, so an expression that reads one
  gives an empty string; `vars` holds every variable as for any run. The read-only forge token,
  `secrets.GITHUB_TOKEN`, arrives with step 34.

The organisations of this installation keep agents in internal networks, so the default fork
policy is `disabled`, and a project that wants fork builds needs an untrusted pool first.

## Drafts

Running a draft requires `workflow.edit`, which only maintainers have by default. A draft run takes
its code from the ref the person chooses, but every rule that looks at the ref sees
`refs/bosun/drafts/<id>`: no environment's branch pattern matches it unless it names it, so a
draft never reaches a protected environment or its secrets the way a reviewed commit to the default
branch would. Organisation and project secrets are available, as they are to any branch a
maintainer pushes.

Saving a draft commits with the user's own authorisation of the App, never the installation's, so
the forge decides whether this person may push to that branch, and branch protection applies.
Without a linked forge account, a draft can be run but not saved.

## Terminals

Opening a debug terminal requires `job.debug`. On the `shell` capability it is a shell on the host
as the agent's user, so the agent must also allow it in its own configuration
(`allow_terminal = true`); this cannot be switched on from the server. Every session is recorded
and linked from the journal and the audit trail.

## What a stolen credential gets

- A personal token gets its keys, within what its creator holds, on the organisations and
  projects it is restricted to, or wherever its creator holds them, until it expires or is revoked.
  It can also make tokens within that reach that expire no later than it does, and its revocation
  leaves those alone.
- An agent credential gets the jobs that would be dispatched to its pool, and with them their
  secrets. Pools limit which projects can use them; credentials rotate; a pool can restrict the
  source addresses its agents may connect from.
- A job token gets the runtime services of one job until its execution completes.

A job can read its own agent's credential: on `shell` the steps run as the agent's user, and with
the podman socket a container can mount the agent's files. Any job dispatched to a pool can
therefore obtain the jobs that later go to that pool. **The pool is the secret boundary**: projects
whose secrets must not be exposed to each other's code belong in different pools, just as for cache
volumes.

## Audit

`audit_event` records the actor, the action, the target, the data with secrets masked, the address,
the user agent and the time, for: sign-ins and failed sign-ins; tokens created and revoked;
secrets and variables written (names, never values); roles and bindings; environments and their
rules; pools, agents and registration tokens; plugins and their configuration; every operator
command, which is in the journal as well; terminal sessions; workflow saves and commits; settings.
Holders of `audit.view` read it in the interface and through the API.
