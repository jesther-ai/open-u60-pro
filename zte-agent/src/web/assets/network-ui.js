(() => {
  const feedback = form => form.querySelector('[data-net-feedback]') || form;
  const reset = form => {
    form.removeAttribute('aria-busy');
    form.querySelectorAll('[data-net-disabled]').forEach(button => {
      button.disabled = false;
      button.removeAttribute('data-net-disabled');
      button.removeAttribute('aria-busy');
    });
    form.querySelectorAll('.net-spin').forEach(spinner => spinner.remove());
    form.querySelectorAll('.net-progress').forEach(message => message.remove());
    form.querySelectorAll('[data-net-spinner-host]').forEach(host => { host.hidden = true; });
    form.querySelectorAll('[data-net-state]').forEach(state => { state.textContent = state.dataset.idleLabel; });
  };
  const note = (form, text, ok = false) => {
    let message = form.querySelector('.net-result');
    if (!message) {
      message = document.createElement('p');
      feedback(form).append(message);
    }
    message.className = `msg ${ok ? 'ok' : 'err'} net-result`;
    message.setAttribute('role', ok ? 'status' : 'alert');
    message.textContent = text;
  };
  document.querySelectorAll('.net-extensions').forEach(form => form.addEventListener('input', event => {
    form.querySelectorAll('.net-result.ok').forEach(message => message.remove());
    const field = event.target.name;
    if (field === 'merge' || field === 'script') form.querySelector(`[data-net-saved="${field}"]`).hidden = true;
  }));
  document.querySelectorAll('dialog[data-auto]').forEach(dialog => {
    if (typeof dialog.showModal === 'function') {
      dialog.removeAttribute('open');
      dialog.showModal();
    }
  });
  document.querySelectorAll('.net-profile').forEach(form => {
    const update = () => {
      const remote = form.elements.source_type.value === 'remote';
      form.querySelectorAll('.net-remote, .net-local').forEach(row => {
        row.hidden = row.classList.contains('net-remote') !== remote;
        row.querySelectorAll('input, select, textarea').forEach(field => { field.disabled = row.hidden; });
      });
      form.elements.url.required = remote;
    };
    form.elements.source_type.addEventListener('change', update);
    update();
  });
  document.addEventListener('click', event => {
    const open = event.target.closest('[data-dialog]');
    const dialog = open && document.getElementById(open.dataset.dialog);
    if (dialog && typeof dialog.showModal === 'function') {
      event.preventDefault();
      dialog.showModal();
    }
    const close = event.target.closest('[data-close]');
    if (close && close.closest('dialog')) {
      event.preventDefault();
      if (!close.closest('dialog').querySelector('[aria-busy="true"]')) {
        close.closest('dialog').close();
        history.replaceState(null, '', '/ui/router/mihomo');
      }
    }
  });
  document.querySelectorAll('dialog').forEach(dialog => dialog.addEventListener('cancel', event => {
    if (dialog.querySelector('[aria-busy="true"]')) event.preventDefault();
  }));
  document.addEventListener('submit', async event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || form.getAttribute('method') !== 'post') return;
    // A named action input shadows form.action on profile forms.
    const url = new URL(form.getAttribute('action'), location.href);
    if (url.origin !== location.origin || !/^\/ui\/(router\/mihomo(?:\/|$)|relay\/)/.test(url.pathname)) return;
    event.preventDefault();
    if (form.getAttribute('aria-busy') === 'true') return;
    if (!form.reportValidity()) return;
    const extensions = form.classList.contains('net-extensions');
    if (extensions) form.closest('details').open = true;
    const button = event.submitter || form.querySelector('button[type="submit"]');
    const data = new FormData(form);
    if (button && button.name) data.append(button.name, button.value);
    form.setAttribute('aria-busy', 'true');
    form.querySelectorAll('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled)').forEach(control => {
      control.setAttribute('data-net-disabled', '');
      control.disabled = true;
    });
    if (button) {
      button.setAttribute('aria-busy', 'true');
      const spinner = document.createElement('span');
      spinner.className = 'spin net-spin';
      spinner.setAttribute('aria-hidden', 'true');
      const host = form.querySelector('[data-net-spinner-host]');
      if (host) { host.hidden = false; host.append(spinner); }
      else button.prepend(spinner);
    }
    form.querySelectorAll('.net-result').forEach(message => message.remove());
    const state = form.querySelector('[data-net-state]');
    const progress = state || document.createElement('p');
    if (!state) {
      progress.className = 'foot net-progress';
      progress.setAttribute('role', 'status');
      feedback(form).append(progress);
    }
    progress.textContent = button?.dataset.busyLabel || 'Working... Please keep this page open.';
    try {
      const multipart = form.enctype === 'multipart/form-data';
      const response = await fetch(url, {
        method: 'POST', credentials: 'same-origin',
        headers: {'X-U60-Form': '1', ...(multipart ? {} : {'Content-Type': 'application/x-www-form-urlencoded'})},
        body: multipart ? data : new URLSearchParams(data)
      });
      if ((response.headers.get('content-type') || '').startsWith('application/json')) {
        const result = await response.json();
        if (result.once && form.elements.once) {
          // Every form from this page shares the original single-use token.
          const previous = data.get('once');
          document.querySelectorAll('form input[name="once"]').forEach(input => {
            if (input.value === previous) input.value = result.once;
          });
        }
        if (result.ok && response.ok) {
          if (!extensions) { location.assign('/ui/router/mihomo'); return; }
          for (const field of ['merge', 'script']) {
            if (typeof result.extensions?.[field] === 'string') {
              form.elements[field].value = result.extensions[field];
              form.elements[field + '_enabled'].checked = result.extensions[field + '_enabled'] === true;
              form.querySelector(`[data-net-saved="${field}"]`).hidden = !result.extensions[field];
            }
          }
          form.closest('details').open = true;
          note(form, result.message || 'Global extensions saved and applied.', true);
          reset(form);
          return;
        }
        note(form, result.error || 'Operation failed. Refresh before retrying.');
      } else if (extensions && (response.ok || response.redirected)) {
        note(form, 'Save was not confirmed; the session may have expired. Your edits are still here. Sign in and verify the saved configuration before retrying.');
      } else if (response.ok || response.redirected) {
        location.assign(response.url);
        return;
      } else {
        const errorPage = new DOMParser().parseFromString(await response.text(), 'text/html');
        const error = errorPage.querySelector('.msg.err, [role="alert"]')?.textContent?.trim();
        note(form, (error ? error.slice(0, 1000) : 'Operation failed or the session expired.') + ' Refresh this page before retrying.');
      }
    } catch (_) {
      note(form, 'Connection interrupted. The router may still be working; refresh and check before retrying.');
    }
    reset(form);
  });
  window.addEventListener('pageshow', () => document.querySelectorAll('form[aria-busy]').forEach(reset));
})()
