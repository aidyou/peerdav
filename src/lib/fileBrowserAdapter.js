import { h } from 'vue';
import { confirmAction } from './confirm';
import { t } from './i18n';
import { invoke } from './invoke';

// Protocol wiring only. Navigation, selection, preview and transfers belong to
// the shared browser; mutation protection remains enforced by the backend.
export function cloudBrowserAdapter(connectionId, bucket, root = '', notifications = {}) {
  return {
    root,
    roots: async () => [{ id: bucket, name: bucket, permissions: { canWrite: false, canDelete: false } }],
    location: () => ({ protocol: 'cloud', connectionId, rootId: bucket }),
    uploadScope: (_share, prefix) => ({ connectionId, bucket, prefix }),
    downloadScope: () => ({ connectionId, bucket }),
    validScope: scope => scope.connectionId === connectionId && scope.bucket === bucket,
    downloadBackend: () => ({ backend: 's3', connectionId }),
    async remove(entry, valid = () => true, sessionId, confirmed = false) {
      if (!confirmed && !await confirmAction(t(entry.isDir ? 'peer.deleteFolderConfirm' : 'peer.deleteFileConfirm', { name: entry.name }), t('common.delete'), { danger: true, confirmText: t('common.delete') })) return false;
      if (!valid()) return false;
      const plan = await invoke('file_delete_plan', { sessionId, id: entry.id });
      if (!valid()) return false;
      if (!plan?.planId || plan.targetId !== entry.id || !['entry', 'snapshot'].includes(plan.confirmation)
        || !Number.isSafeInteger(plan.objectCount) || plan.objectCount < 0 || !Array.isArray(plan.entries)
        || plan.entries.length !== plan.objectCount || !Number.isFinite(plan.expiresAtUnix)
        || new Set(plan.entries.map(item => item?.id)).size !== plan.objectCount
        || plan.entries.some(item => typeof item?.id !== 'string' || !item.id
          || !Number.isSafeInteger(item.size) || item.size < 0 || (item.revision !== null && typeof item.revision !== 'string'))
        || (plan.confirmation === 'entry' && (plan.objectCount !== 1 || plan.entries[0].id !== entry.id))) {
        throw { code: 'CLOUD_CONFIRMATION_REQUIRED', message: t('cloudBrowser.invalidPlan') };
      }
      if (plan.confirmation === 'entry') {
        if (Date.now() >= plan.expiresAtUnix * 1000) throw { code: 'CLOUD_DELETE_PLAN_STALE', message: t('cloudBrowser.stalePlan') };
        return invoke('file_delete_execute', { sessionId, planId: plan.planId, confirmed: true });
      }
      const body = h('div', [
        h('div', { class: 'confirm-heading' }, [
          h('span', { class: 'confirm-heading__icon' }, [h('svg', { viewBox: '0 0 16 16', width: 20, height: 20, 'aria-hidden': 'true' }, [h('circle', { cx: 8, cy: 8, r: 7, fill: 'currentColor' }), h('path', { d: 'M8 4.4v4.2M8 11.2v.01', stroke: '#fff', 'stroke-width': 1.7, 'stroke-linecap': 'round' })])]),
          h('span', t('cloudBrowser.deleteTitle')),
        ]),
        h('p', { class: 'confirm-summary' }, t('cloudBrowser.deleteSummary', { count: plan.objectCount })),
        h('p', { class: 'confirm-summary' }, t('cloudBrowser.deleteConcurrency')),
        h('div', { class: 'confirm-scope', 'data-test': 'delete-target' }, entry.id),
        h('ul', { class: 'confirm-list', 'data-test': 'delete-keys' }, plan.entries.map(item => h('li', { key: item.id }, /[\n\r\t"]/.test(item.id) || item.id !== item.id.trim() ? JSON.stringify(item.id) : item.id))),
      ]);
      if (!await confirmAction(body, '', { headerless: true, type: null, danger: true, confirmText: t('common.delete') }) || !valid()) return false;
      if (Date.now() >= plan.expiresAtUnix * 1000) throw { code: 'CLOUD_DELETE_PLAN_STALE', message: t('cloudBrowser.stalePlan') };
      const result = await invoke('file_delete_execute', { sessionId, planId: plan.planId, confirmed: true });
      if (!valid()) return false;
      if (result.failed.length) notifications.setError?.('cloudBrowser.deletePartial', { deleted: result.deleted.length, failed: result.failed.length, reason: result.failed[0].error?.message || t('cloudBrowser.deleteFailedUnknown') });
      else if (result.deleted.length) notifications.setNotice?.('cloudBrowser.deletedObjects', { count: result.deleted.length });
      return result;
    },
  };
}
