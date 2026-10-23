import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { App } from '../app';
import type { Member } from '../api';
import { setLocale } from '../i18n';
import en from '../i18n/en.json';
import th from '../i18n/th.json';
import { deferred, mockApi, type Reply } from '../testing/api-mock';
import { installFakeEventSource } from '../testing/fake-event-source';
import { ME, SPACE, TEAM, THAI_TEAM, WORKSPACE } from '../testing/fixtures';
import { Members } from './Members';

const BASE = `/api/workspaces/${WORKSPACE}`;
const LIST = `GET ${BASE}/members`;
const INVITE = `POST ${BASE}/members`;
const role = (user: string) => `PUT ${BASE}/members/${user}/role`;
const OPTIONS = `GET ${BASE}/members/invite-options`;

const PEOPLE: Member[] = [
  { user_id: 'user-2', email: 'malee@example.test', display_name: 'มาลี', role: 'member' },
  { user_id: ME, email: 'pat@example.test', display_name: 'Pat', role: 'owner' },
];

beforeEach(() => {
  installFakeEventSource();
  window.history.replaceState(null, '', '/');
});

afterEach(() => {
  vi.unstubAllGlobals();
  act(() => setLocale('en'));
});

function renderMembers(myRole: Member['role'] = 'owner', onOwnRoleChanged = vi.fn()) {
  render(
    <Members
      workspaceId={WORKSPACE}
      workspaceName="เดโม"
      me={ME}
      myRole={myRole}
      onOwnRoleChanged={onOwnRoleChanged}
    />,
  );
  return onOwnRoleChanged;
}

test.each(['member', 'admin', 'owner'] as const)(
  'members_page_is_offered_only_to_owners_and_admins (%s)',
  async (myRole) => {
    mockApi({
      'GET /api/auth/me': [200, { user_id: ME }],
      'GET /api/workspaces': [200, [{ id: WORKSPACE, name: 'เดโม', role: myRole, paused: false }]],
      [`GET ${BASE}/spaces`]: [200, [{ id: SPACE, name: 'พื้นที่ทดลอง' }]],
      [`GET ${BASE}/spaces/${SPACE}/teams`]: [200, [THAI_TEAM.team]],
      [`GET ${BASE}/spaces/${SPACE}/threads`]: [200, []],
      [`GET ${BASE}/teams/${TEAM}`]: [200, THAI_TEAM],
      [LIST]: [200, PEOPLE],
    });
    render(<App />);
    const nav = await screen.findByRole('navigation', { name: en['nav.main'] });
    await within(nav).findByRole('link', { name: en['nav.chat'] });
    const link = within(nav).queryByRole('link', { name: en['nav.members'] });
    if (myRole === 'member') {
      expect(link).toBeNull();
      // Typed in by hand, the page still refuses.
      window.history.pushState(null, '', '/members');
      act(() => window.dispatchEvent(new PopStateEvent('popstate')));
      expect(await screen.findByText(en['reason.forbidden'])).toBeVisible();
    } else {
      await userEvent.setup({ delay: null }).click(link!);
      expect(
        await screen.findByRole('heading', { level: 1, name: en['members.title'] }),
      ).toBeVisible();
      expect(await screen.findByText('malee@example.test')).toBeVisible();
    }
  },
);

test('members_page_lists_people_with_their_roles', async () => {
  mockApi({ [LIST]: [200, PEOPLE] });
  renderMembers();
  const table = await screen.findByRole('table', {
    name: en['members.list'].replace('{workspace}', 'เดโม'),
  });
  const rows = within(table).getAllByRole('row').slice(1);
  expect(rows.map((row) => within(row).getAllByRole('cell')[1]!.textContent)).toEqual([
    'malee@example.test',
    'pat@example.test',
  ]);
  expect(within(rows[1]!).getByText(en['members.you'])).toBeVisible();
  expect(
    within(rows[0]!).getByRole('combobox', {
      name: en['members.role_for'].replace('{name}', 'มาลี'),
    }),
  ).toHaveValue('member');
});

test('members_page_invites_by_link_and_shows_it_once', async () => {
  const link = `http://localhost:3000/invite?token=${crypto.randomUUID()}`;
  const calls = mockApi({
    [LIST]: [200, PEOPLE],
    [INVITE]: [201, { user_id: 'user-3', created: true, link }],
  });
  // user-event installs a clipboard the test can read back.
  const user = userEvent.setup({ delay: null });
  const { unmount } = render(
    <Members workspaceId={WORKSPACE} workspaceName="เดโม" me={ME} myRole="admin" />,
  );

  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  await user.type(within(form).getByLabelText(en['members.invite.email']), 'somchai@example.test');
  await user.selectOptions(within(form).getByLabelText(en['members.invite.role']), 'admin');
  // An admin cannot invite an owner.
  expect(
    within(within(form).getByLabelText(en['members.invite.role'])).queryByRole('option', {
      name: en['members.role.owner'],
    }),
  ).toBeNull();
  await user.click(within(form).getByRole('radio', { name: en['members.invite.by_link'] }));
  await user.click(within(form).getByRole('button', { name: en['members.invite.submit'] }));

  expect(calls.callsTo(INVITE)[0]!.body).toEqual({
    email: 'somchai@example.test',
    role: 'admin',
    delivery: 'link',
  });
  const shown = await screen.findByRole('region', {
    name: en['members.invite.link_title'].replace('{email}', 'somchai@example.test'),
  });
  expect(within(shown).getByRole('textbox')).toHaveValue(link);
  await user.click(within(shown).getByRole('button', { name: en['members.invite.copy'] }));
  expect(await navigator.clipboard.readText()).toBe(link);
  expect(await within(shown).findByText(en['members.invite.copied'])).toBeVisible();
  // The list is read again so the new person shows.
  expect(calls.callsTo(LIST)).toHaveLength(2);

  // Gone once the person leaves the page: nothing keeps it.
  unmount();
  expect(JSON.stringify({ ...localStorage })).not.toContain(link);
  expect(window.location.href).not.toContain('token');
  renderMembers('admin');
  await screen.findByRole('form', { name: en['members.invite.title'] });
  expect(screen.queryByDisplayValue(link)).toBeNull();
});

test('members_page_says_an_existing_account_was_added_without_a_link', async () => {
  mockApi({
    [LIST]: [200, PEOPLE],
    [INVITE]: [201, { user_id: 'user-9', created: false, link: null }],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  await user.type(within(form).getByLabelText(en['members.invite.email']), 'old@example.test');
  await user.click(within(form).getByRole('radio', { name: en['members.invite.by_link'] }));
  await user.click(within(form).getByRole('button', { name: en['members.invite.submit'] }));
  expect(
    await screen.findByText(en['members.invite.added'].replace('{email}', 'old@example.test')),
  ).toBeVisible();
  expect(screen.queryByRole('button', { name: en['members.invite.copy'] })).toBeNull();
});

test('members_page_explains_email_invites_without_smtp', async () => {
  const calls = mockApi({
    [LIST]: [200, PEOPLE],
    [OPTIONS]: [200, { email: false }],
    [INVITE]: [422, { code: 'smtp_unconfigured' }],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  // Without SMTP the form starts on the link, and says why.
  await waitFor(() =>
    expect(within(form).getByRole('radio', { name: en['members.invite.by_link'] })).toBeChecked(),
  );
  expect(within(form).getByText(en['members.invite.email_unavailable'])).toBeVisible();
  await user.type(within(form).getByLabelText(en['members.invite.email']), 'somchai@example.test');
  await user.click(within(form).getByRole('radio', { name: en['members.invite.by_email'] }));
  await user.click(within(form).getByRole('button', { name: en['members.invite.submit'] }));
  expect(calls.callsTo(INVITE)[0]!.body).toMatchObject({ delivery: 'email' });
  const alert = await screen.findByRole('alert');
  expect(alert).toHaveTextContent(en['reason.smtp_unconfigured']);
  expect(alert).toHaveTextContent(en['members.invite.by_link']);

  act(() => setLocale('th'));
  expect(screen.getByRole('alert')).toHaveTextContent(th['reason.smtp_unconfigured']);
  expect(th['reason.smtp_unconfigured']).toContain(th['members.invite.by_link']);
});

test('members_page_changes_a_role', async () => {
  const calls = mockApi({
    [LIST]: [200, PEOPLE],
    [role('user-2')]: [204],
    [role(ME)]: [422, { code: 'last_owner_required' }],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const malee = await screen.findByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'มาลี'),
  });
  await user.selectOptions(malee, 'admin');
  expect(calls.callsTo(role('user-2'))[0]!.body).toEqual({ role: 'admin' });
  expect(
    await screen.findByText(
      en['members.role_saved']
        .replace('{name}', 'มาลี')
        .replace('{role}', en['members.role.admin']),
    ),
  ).toBeVisible();
  expect(malee).toHaveValue('admin');

  // The last owner stays an owner, and the select shows the role the server kept.
  const pat = screen.getByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'Pat'),
  });
  await user.selectOptions(pat, 'member');
  // Lowering your own role is confirmed first.
  await user.click(screen.getByRole('button', { name: en['members.self_role_yes'] }));
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.last_owner_required']);
  await waitFor(() => expect(pat).toHaveValue('owner'));
});

test('members_page_removes_a_member_after_confirming', async () => {
  const calls = mockApi({
    [LIST]: [
      [200, PEOPLE],
      [200, PEOPLE.slice(1)],
    ],
    [`DELETE ${BASE}/members/user-2`]: [204],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  await user.click(
    await screen.findByRole('button', {
      name: en['members.remove_label'].replace('{name}', 'มาลี'),
    }),
  );
  // Nothing happens until the person confirms.
  expect(calls.callsTo(`DELETE ${BASE}/members/user-2`)).toHaveLength(0);
  const confirm = screen.getByRole('group', {
    name: en['members.remove_label'].replace('{name}', 'มาลี'),
  });
  expect(confirm).toHaveTextContent(en['members.remove_confirm'].replace('{name}', 'มาลี'));
  await user.click(within(confirm).getByRole('button', { name: en['members.remove_yes'] }));
  expect(calls.callsTo(`DELETE ${BASE}/members/user-2`)).toHaveLength(1);
  expect(await screen.findByText(en['members.removed'].replace('{name}', 'มาลี'))).toBeVisible();
  await waitFor(() => expect(screen.queryByText('malee@example.test')).toBeNull());
  // Nobody removes themselves here.
  expect(
    screen.queryByRole('button', { name: en['members.remove_label'].replace('{name}', 'Pat') }),
  ).toBeNull();
});

test('invite_form_starts_on_email_when_the_server_can_mail', async () => {
  mockApi({ [LIST]: [200, PEOPLE], [OPTIONS]: [200, { email: true }] });
  renderMembers();
  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  await waitFor(() =>
    expect(within(form).getByRole('radio', { name: en['members.invite.by_email'] })).toBeChecked(),
  );
  expect(within(form).queryByText(en['members.invite.email_unavailable'])).toBeNull();
});

test('a_shown_invite_link_says_how_to_get_a_new_one', async () => {
  mockApi({
    [LIST]: [200, PEOPLE],
    [OPTIONS]: [200, { email: false }],
    [INVITE]: [
      201,
      { user_id: 'user-3', created: true, link: 'http://localhost:3000/invite?token=t' },
    ],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  await user.type(within(form).getByLabelText(en['members.invite.email']), 'new@example.test');
  await user.click(within(form).getByRole('button', { name: en['members.invite.submit'] }));
  expect(await screen.findByText(en['members.invite.link_lost'])).toBeVisible();
});

test('overlapping_role_changes_send_one_at_a_time_and_the_latest_wins', async () => {
  const first = deferred<Reply>();
  const sent: unknown[] = [];
  const calls = mockApi({ [LIST]: [200, PEOPLE] });
  calls.route(role('user-2'), ({ body }) => {
    sent.push(body);
    return sent.length === 1 ? first.promise : [204];
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const malee = await screen.findByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'มาลี'),
  });
  await user.selectOptions(malee, 'admin');
  await user.selectOptions(malee, 'owner');
  // The second change waits for the first instead of racing it.
  expect(sent).toEqual([{ role: 'admin' }]);
  expect(malee).toHaveValue('owner');

  // The first fails, but a newer choice is queued: it is sent, and it is what the page shows.
  await act(async () => first.resolve([503, { code: 'busy' }]));
  await waitFor(() => expect(sent).toEqual([{ role: 'admin' }, { role: 'owner' }]));
  await waitFor(() => expect(malee).toHaveValue('owner'));
  expect(screen.queryByRole('alert')).toBeNull();
});

test('a_failed_latest_role_change_shows_the_role_the_server_kept', async () => {
  const first = deferred<Reply>();
  const sent: unknown[] = [];
  const calls = mockApi({ [LIST]: [200, PEOPLE] });
  calls.route(role('user-2'), ({ body }) => {
    sent.push(body);
    return sent.length === 1 ? first.promise : [422, { code: 'last_owner_required' }];
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  const malee = await screen.findByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'มาลี'),
  });
  await user.selectOptions(malee, 'admin');
  await user.selectOptions(malee, 'owner');
  await act(async () => first.resolve([204]));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.last_owner_required']);
  await waitFor(() => expect(malee).toHaveValue('admin'));
});

test('demoting_yourself_asks_first_then_refreshes_the_page', async () => {
  const admins: Member[] = [
    { user_id: 'user-2', email: 'malee@example.test', display_name: 'มาลี', role: 'owner' },
    { user_id: ME, email: 'pat@example.test', display_name: 'Pat', role: 'admin' },
  ];
  const calls = mockApi({ [LIST]: [200, admins], [role(ME)]: [204] });
  const user = userEvent.setup({ delay: null });
  const onOwnRoleChanged = renderMembers('admin');
  const mine = await screen.findByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'Pat'),
  });
  await user.selectOptions(mine, 'member');
  const confirm = screen.getByRole('group', { name: en['members.self_role'] });
  expect(confirm).toHaveTextContent(
    en['members.self_role_confirm'].replace('{role}', en['members.role.member']),
  );
  expect(within(confirm).getByRole('button', { name: en['members.cancel'] })).toHaveFocus();
  expect(calls.callsTo(role(ME))).toHaveLength(0);
  expect(mine).toHaveValue('admin');

  await user.click(within(confirm).getByRole('button', { name: en['members.cancel'] }));
  expect(screen.queryByRole('group', { name: en['members.self_role'] })).toBeNull();
  expect(mine).toHaveValue('admin');
  expect(mine).toHaveFocus();

  await user.selectOptions(mine, 'member');
  await user.click(screen.getByRole('button', { name: en['members.self_role_yes'] }));
  expect(calls.callsTo(role(ME))[0]!.body).toEqual({ role: 'member' });
  await waitFor(() => expect(onOwnRoleChanged).toHaveBeenCalledOnce());
});

test('focus_lands_on_the_outcome_after_removing_a_member', async () => {
  mockApi({
    [LIST]: [
      [200, PEOPLE],
      [200, PEOPLE.slice(1)],
    ],
    [`DELETE ${BASE}/members/user-2`]: [204],
  });
  const user = userEvent.setup({ delay: null });
  renderMembers();
  await user.click(
    await screen.findByRole('button', {
      name: en['members.remove_label'].replace('{name}', 'มาลี'),
    }),
  );
  await user.click(screen.getByRole('button', { name: en['members.remove_yes'] }));
  const outcome = await screen.findByText(en['members.removed'].replace('{name}', 'มาลี'));
  await waitFor(() => expect(outcome).toHaveFocus());
});

test('a_successful_self_demotion_refreshes_permissions_even_when_a_queued_change_fails', async () => {
  const admins: Member[] = [
    { user_id: 'user-2', email: 'malee@example.test', display_name: 'มาลี', role: 'owner' },
    { user_id: ME, email: 'pat@example.test', display_name: 'Pat', role: 'admin' },
  ];
  const demotion = deferred<Reply>();
  const sent: unknown[] = [];
  const calls = mockApi({ [LIST]: [200, admins] });
  calls.route(role(ME), ({ body }) => {
    sent.push(body);
    // Once demoted, the person is a member: their own re-promotion is refused.
    return sent.length === 1 ? demotion.promise : [403, { code: 'forbidden' }];
  });
  const user = userEvent.setup({ delay: null });
  const onOwnRoleChanged = renderMembers('admin');
  const mine = await screen.findByRole('combobox', {
    name: en['members.role_for'].replace('{name}', 'Pat'),
  });
  await user.selectOptions(mine, 'member');
  await user.click(screen.getByRole('button', { name: en['members.self_role_yes'] }));
  await user.selectOptions(mine, 'admin');
  expect(sent).toEqual([{ role: 'member' }]);

  await act(async () => demotion.resolve([204]));
  await waitFor(() => expect(sent).toEqual([{ role: 'member' }, { role: 'admin' }]));
  expect(await screen.findByRole('alert')).toHaveTextContent(en['reason.forbidden']);
  // The demotion went through, so the page must follow it whatever the queued change did.
  expect(onOwnRoleChanged).toHaveBeenCalledOnce();
  await waitFor(() => expect(mine).toHaveValue('member'));
});

test('invite_options_failing_to_load_makes_no_claim_about_email', async () => {
  mockApi({ [LIST]: [200, PEOPLE], [OPTIONS]: [503, { code: 'database_unavailable' }] });
  renderMembers();
  const form = await screen.findByRole('form', { name: en['members.invite.title'] });
  expect(await within(form).findByText(en['members.invite.options_failed'])).toBeVisible();
  expect(within(form).queryByText(en['members.invite.email_unavailable'])).toBeNull();
  expect(within(form).getByRole('radio', { name: en['members.invite.by_link'] })).toBeChecked();
  expect(within(form).getByRole('radio', { name: en['members.invite.by_email'] })).toBeEnabled();
});
