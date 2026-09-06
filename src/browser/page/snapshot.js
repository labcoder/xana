// Fixed first-party inspection only; callers cannot supply a function body.
// Do not read secret, hidden or file values, even for hashing/fingerprinting.
function snapshot(element) {
    const keys = ['id', 'name', 'type', 'href', 'action', 'formaction', 'method',
        'formmethod', 'formtarget', 'formenctype', 'formnovalidate', 'form', 'role',
        'aria-label', 'aria-labelledby', 'aria-describedby', 'disabled'];
    if (!element.isConnected || element.tagName === 'INPUT' &&
        ['password', 'file', 'hidden'].includes(element.type)) return { unsupported: true };
    const label = ['INPUT', 'TEXTAREA'].includes(element.tagName) ? '' : element.textContent || '';
    const attributes = keys.map(key => [key, element.getAttribute(key)]);
    const destination = element.tagName === 'A' ? element.href : null;
    if (destination && destination.length > 2048) return { unsupported: true };
    if (label.length > 1024 || attributes.some(([, value]) => value && value.length > 1024) ||
        (element.labels?.length || 0) > 8) return { unsupported: true };
    const labels = Array.from(element.labels || []).map(label => label.textContent || '');
    const labelledBy = (element.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean);
    if (labelledBy.length > 8) return { unsupported: true };
    for (const id of labelledBy) labels.push(element.ownerDocument.getElementById(id)?.textContent || '');
    if (labels.some(value => value.length > 1024)) return { unsupported: true };

    let form = null;
    if (element.form) {
        const owner = element.form;
        if (owner.elements.length > 32) return { tag: element.tagName, label, labels, attributes, destination, form: { unsupported: true } };
        const elements = Array.from(owner.elements);
        // Ambiguous/custom/secret-bearing forms require explicit manual control.
        if (elements.length > 32 || elements.some(field =>
            !['INPUT', 'TEXTAREA', 'SELECT', 'BUTTON'].includes(field.tagName) ||
            ['password', 'file', 'hidden'].includes(field.type) ||
            /password|secret|token|csrf|credential|api.?key|authorization/i.test(field.name || '') ||
            /cc-|password|one-time-code/.test(field.autocomplete || ''))) {
            form = { unsupported: true };
        } else {
            const fields = [];
            for (const field of elements) {
                if ((field.name || '').length > 512 ||
                    field.tagName === 'SELECT' && field.selectedOptions.length > 32) return { unsupported: true };
                const value = field.tagName === 'SELECT'
                    ? Array.from(field.selectedOptions).map(option => option.value)
                    : field.value || '';
                if ((Array.isArray(value) ? value : [value]).some(value => value.length > 4096)) return { unsupported: true };
                fields.push({
                    tag: field.tagName, type: field.type || '', name: field.name || '',
                    value,
                    checked: field.checked === true, disabled: field.matches(':disabled'),
                    readOnly: field.readOnly === true,
                });
            }
            form = {
                action: element.hasAttribute('formaction') ? element.formAction : owner.action,
                method: element.hasAttribute('formmethod') ? element.formMethod : owner.method,
                enctype: element.hasAttribute('formenctype') ? element.formEnctype : owner.enctype,
                target: element.hasAttribute('formtarget') ? element.formTarget : owner.target,
                noValidate: owner.noValidate || element.formNoValidate === true,
                fields,
            };
            if (JSON.stringify(form).length > 8192) form = { unsupported: true };
        }
    }
    return { tag: element.tagName, label, labels, attributes, destination, form };
}
