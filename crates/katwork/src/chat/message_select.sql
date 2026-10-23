SELECT m.id, m.thread_id,
    jsonb_build_object('kind', m.author_kind) || CASE WHEN m.author_kind='system' THEN '{}'::jsonb
        ELSE jsonb_build_object('id', coalesce(m.author_user_id, m.author_kat_id)) END AS author,
    coalesce(u.display_name, k.name, '') AS author_name,
    m.body, m.synthetic, m.stopped, m.question, m.meta, m.job_id,
    to_jsonb(j.status) AS job_status, to_jsonb(j.reason) AS job_reason, m.client_msg_id, m.created_at
    FROM messages m
    LEFT JOIN users u ON u.id=m.author_user_id
    LEFT JOIN kats k ON k.id=m.author_kat_id AND k.workspace_id=m.workspace_id
    LEFT JOIN jobs j ON j.id=m.job_id AND j.workspace_id=m.workspace_id
    WHERE m.workspace_id=$1 AND m.thread_id=$2
