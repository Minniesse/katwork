WITH ranked AS MATERIALIZED (
    SELECT m.id, coalesce(j.request_message_id,m.id) AS position,
        sum(char_length(m.body)) OVER (
            ORDER BY coalesce(j.request_message_id,m.id) DESC,m.id DESC
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS chars
    FROM messages m LEFT JOIN jobs j ON j.id=m.job_id AND j.workspace_id=m.workspace_id
    WHERE m.workspace_id=$1 AND m.thread_id=$2 AND m.author_kind<>'system'
      AND (coalesce(j.request_message_id,m.id)<$3 OR m.id=$3)
      AND EXISTS (SELECT 1 FROM messages request WHERE request.workspace_id=$1
                  AND request.thread_id=$2 AND request.id=$3 AND request.author_kind='user')
), kept AS MATERIALIZED (SELECT id,position FROM ranked WHERE chars<=$4 OR id=$3)
SELECT m.id,m.thread_id,
    jsonb_build_object('kind',m.author_kind,'id',coalesce(m.author_user_id,m.author_kat_id)) AS author,
    coalesce(u.display_name,k.name,'') AS author_name,
    m.body,m.synthetic,m.stopped,m.question,m.meta,m.job_id,
    to_jsonb(j.status) AS job_status,to_jsonb(j.reason) AS job_reason,m.client_msg_id,m.created_at,
    (SELECT count(*) FROM ranked)-(SELECT count(*) FROM kept) AS dropped
FROM kept h JOIN messages m ON m.id=h.id AND m.workspace_id=$1 AND m.thread_id=$2
LEFT JOIN users u ON u.id=m.author_user_id
LEFT JOIN kats k ON k.id=m.author_kat_id AND k.workspace_id=m.workspace_id
LEFT JOIN jobs j ON j.id=m.job_id AND j.workspace_id=m.workspace_id
ORDER BY h.position,m.id
