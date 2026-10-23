import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import {
  api,
  errorMessage,
  type InviteDelivery,
  type InviteResult,
  type Member,
  type Role,
} from '../api';
import { PageTitle } from '../components/Layout';
import { t, useLocale, type Vars } from '../i18n';

interface Props {
  workspaceId: string;
  workspaceName: string;
  me: string;
  /** The signed-in user's role here (`WorkspaceSummary.role`). */
  myRole: Role;
}

type Message = { key: string; vars?: Vars };

/** Owners and admins: who is in the workspace, their roles, and invites. */
export function Members({ workspaceId, workspaceName, me, myRole }: Props) {
  useLocale();
  const [members, setMembers] = useState<Member[] | null>(null);
  const [failure, setFailure] = useState<unknown>(null);
  const [saved, setSaved] = useState<Message | null>(null);
  const [reloads, setReloads] = useState(0);
  const allowed = myRole !== 'member';

  useEffect(() => {
    if (!allowed) return;
    let live = true;
    api.members(workspaceId).then(
      (list) => live && setMembers(list),
      (error: unknown) => live && setFailure(error),
    );
    return () => {
      live = false;
    };
  }, [workspaceId, reloads, allowed]);

  if (!allowed) {
    return (
      <div className="page">
        <PageTitle>{t('members.title')}</PageTitle>
        <div className="alert" role="alert">
          <p>{t('reason.forbidden')}</p>
        </div>
      </div>
    );
  }

  const changeRole = async (member: Member, role: Role) => {
    setFailure(null);
    setSaved(null);
    setMembers(
      (list) =>
        list?.map((other) => (other.user_id === member.user_id ? { ...other, role } : other)) ??
        list,
    );
    try {
      await api.setRole(workspaceId, member.user_id, role);
      setSaved({
        key: 'members.role_saved',
        vars: { name: member.display_name, role: `members.role.${role}` },
      });
    } catch (error) {
      setFailure(error);
      // The server kept the old role; show it.
      setMembers(
        (list) => list?.map((other) => (other.user_id === member.user_id ? member : other)) ?? list,
      );
    }
  };

  const remove = async (member: Member) => {
    setFailure(null);
    setSaved(null);
    try {
      await api.removeMember(workspaceId, member.user_id);
      setSaved({ key: 'members.removed', vars: { name: member.display_name } });
      setReloads((n) => n + 1);
    } catch (error) {
      setFailure(error);
    }
  };

  return (
    <div className="page">
      <div className="page-intro">
        <PageTitle>{t('members.title')}</PageTitle>
        <p className="lede">{t('members.lede')}</p>
      </div>
      <p className="notice-line" role="status">
        {saved && t(saved.key, translatedVars(saved.vars))}
      </p>
      {failure !== null && (
        <div className="alert" role="alert">
          <p>{errorMessage(failure)}</p>
        </div>
      )}
      {members === null ? (
        failure === null && <p className="hint">{t('members.loading')}</p>
      ) : (
        <MemberTable
          members={members}
          workspaceName={workspaceName}
          me={me}
          myRole={myRole}
          onRole={changeRole}
          onRemove={remove}
        />
      )}
      <InviteForm
        workspaceId={workspaceId}
        myRole={myRole}
        onInvited={() => setReloads((n) => n + 1)}
      />
    </div>
  );
}

// A role name inside a message is itself a key, translated at render time with the locale.
function translatedVars(vars: Vars | undefined): Vars | undefined {
  if (!vars) return vars;
  return Object.fromEntries(
    Object.entries(vars).map(([name, value]) => [
      name,
      typeof value === 'string' && value.startsWith('members.role.') ? t(value) : value,
    ]),
  );
}

function roleOptions(myRole: Role, current: Role): Role[] {
  // Only an owner may make someone an owner; an owner's row still shows its role.
  return myRole === 'owner' || current === 'owner'
    ? ['member', 'admin', 'owner']
    : ['member', 'admin'];
}

function MemberTable({
  members,
  workspaceName,
  me,
  myRole,
  onRole,
  onRemove,
}: {
  members: Member[];
  workspaceName: string;
  me: string;
  myRole: Role;
  onRole: (member: Member, role: Role) => void;
  onRemove: (member: Member) => void;
}) {
  useLocale();
  const [confirming, setConfirming] = useState<string | null>(null);
  return (
    <table className="members-table">
      <caption>{t('members.list', { workspace: workspaceName })}</caption>
      <thead>
        <tr>
          <th scope="col">{t('members.name')}</th>
          <th scope="col">{t('members.email')}</th>
          <th scope="col">{t('members.role')}</th>
          <th scope="col">{t('members.actions')}</th>
        </tr>
      </thead>
      <tbody>
        {members.map((member) => {
          const self = member.user_id === me;
          const ownerOnly = member.role === 'owner' && myRole !== 'owner';
          return (
            <tr key={member.user_id}>
              <td>
                {member.display_name} {self && <span className="hint">{t('members.you')}</span>}
              </td>
              <td>{member.email}</td>
              <td>
                <select
                  className="select"
                  aria-label={t('members.role_for', { name: member.display_name })}
                  value={member.role}
                  disabled={ownerOnly}
                  onChange={(event) => onRole(member, event.target.value as Role)}
                >
                  {roleOptions(myRole, member.role).map((role) => (
                    <option key={role} value={role}>
                      {t(`members.role.${role}`)}
                    </option>
                  ))}
                </select>
              </td>
              <td>
                {!self &&
                  !ownerOnly &&
                  (confirming === member.user_id ? (
                    <ConfirmRemove
                      member={member}
                      onConfirm={() => {
                        setConfirming(null);
                        onRemove(member);
                      }}
                      onCancel={() => setConfirming(null)}
                    />
                  ) : (
                    <button
                      type="button"
                      className="button button-quiet"
                      aria-label={t('members.remove_label', { name: member.display_name })}
                      onClick={() => setConfirming(member.user_id)}
                    >
                      {t('members.remove')}
                    </button>
                  ))}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/** An inline confirmation, not a browser dialog; focus starts on Cancel. */
function ConfirmRemove({
  member,
  onConfirm,
  onCancel,
}: {
  member: Member;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const cancel = useRef<HTMLButtonElement>(null);
  useEffect(() => cancel.current?.focus(), []);
  return (
    <div
      className="confirm"
      role="group"
      aria-label={t('members.remove_label', { name: member.display_name })}
    >
      <p>{t('members.remove_confirm', { name: member.display_name })}</p>
      <div className="form-actions">
        <button type="button" className="button button-danger" onClick={onConfirm}>
          {t('members.remove_yes')}
        </button>
        <button ref={cancel} type="button" className="button button-quiet" onClick={onCancel}>
          {t('members.cancel')}
        </button>
      </div>
    </div>
  );
}

type Outcome = { email: string; result: InviteResult; delivery: InviteDelivery };

function InviteForm({
  workspaceId,
  myRole,
  onInvited,
}: {
  workspaceId: string;
  myRole: Role;
  onInvited: () => void;
}) {
  useLocale();
  const id = useId();
  const [email, setEmail] = useState('');
  const [role, setRole] = useState<Role>('member');
  const [delivery, setDelivery] = useState<InviteDelivery>('email');
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);
  // Held in memory only: a returned link is shown once and is gone when the page unmounts.
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true);
    setFailure(null);
    setOutcome(null);
    try {
      const result = await api.invite(workspaceId, { email, role, delivery });
      setOutcome({ email, result, delivery });
      setEmail('');
      onInvited();
    } catch (error) {
      setFailure(error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="panel invite">
      <form className="form" onSubmit={submit} aria-labelledby={`${id}-title`}>
        <h2 id={`${id}-title`}>{t('members.invite.title')}</h2>
        <div className="field">
          <label htmlFor={`${id}-email`}>{t('members.invite.email')}</label>
          <input
            id={`${id}-email`}
            type="email"
            autoComplete="off"
            required
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${id}-role`}>{t('members.invite.role')}</label>
          <select
            id={`${id}-role`}
            className="select"
            value={role}
            onChange={(event) => setRole(event.target.value as Role)}
          >
            {roleOptions(myRole, 'member').map((option) => (
              <option key={option} value={option}>
                {t(`members.role.${option}`)}
              </option>
            ))}
          </select>
        </div>
        <fieldset className="choices">
          <legend>{t('members.invite.delivery')}</legend>
          {(['email', 'link'] as const).map((option) => (
            <label key={option} className="choice">
              <input
                type="radio"
                name={`${id}-delivery`}
                value={option}
                checked={delivery === option}
                onChange={() => setDelivery(option)}
              />
              {t(option === 'email' ? 'members.invite.by_email' : 'members.invite.by_link')}
            </label>
          ))}
        </fieldset>
        {failure !== null && (
          <div className="alert" role="alert">
            <p>{errorMessage(failure)}</p>
          </div>
        )}
        <div className="form-actions">
          <button type="submit" className="button button-primary" disabled={busy}>
            {busy ? t('members.invite.sending') : t('members.invite.submit')}
          </button>
        </div>
      </form>
      {outcome && <InviteOutcome outcome={outcome} />}
    </div>
  );
}

function InviteOutcome({ outcome }: { outcome: Outcome }) {
  useLocale();
  const id = useId();
  const [copied, setCopied] = useState(false);
  const field = useRef<HTMLInputElement>(null);
  const { email, result } = outcome;
  if (!result.created) {
    return (
      <p className="notice-line" role="status">
        {t('members.invite.added', { email })}
      </p>
    );
  }
  if (!result.link) {
    return (
      <p className="notice-line" role="status">
        {t('members.invite.emailed', { email })}
      </p>
    );
  }
  const link = result.link;
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
    } catch {
      // No clipboard access: select the link so the person can copy it themselves.
      field.current?.select();
    }
  };
  return (
    <section className="invite-link" aria-labelledby={`${id}-title`}>
      <h3 id={`${id}-title`}>{t('members.invite.link_title', { email })}</h3>
      <p className="hint">{t('members.invite.link_hint')}</p>
      <div className="invite-link-row">
        <input
          ref={field}
          className="invite-link-field"
          readOnly
          value={link}
          aria-labelledby={`${id}-title`}
          onFocus={(event) => event.target.select()}
        />
        <button type="button" className="button button-primary" onClick={() => void copy()}>
          {t('members.invite.copy')}
        </button>
      </div>
      <p role="status">{copied && t('members.invite.copied')}</p>
    </section>
  );
}
