SELECT coalesce(sum(charged_tokens),0)::bigint FROM model_calls
WHERE workspace_id=$1 AND kat_id=$2 AND created_at>statement_timestamp()-interval '24 hours'
