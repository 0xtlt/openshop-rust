/*
 * openshop.js: the script of the admin pages the server renders. Written by hand, embedded in the
 * binary, no dependency and no build. The server does the work; this script only presents it.
 *
 * - A form is sent in the background with App Bridge's fetch, which adds the session token. Its
 *   submit `s-button` has `loading` while the request runs. A form with `data-openshop-native` is
 *   left to the browser. Enter in a Polaris field submits its form, as in a native field; a
 *   `GET` form with `data-openshop-autosubmit` is sent when one of its fields changes.
 * - A link of the app (`<a>`, and the `s-link` and `s-button` Polaris announces with
 *   `shopify:navigate`) is fetched the same way, and back and forward too.
 * - What the server answers replaces the content of `#openshop-page`, the title, and the menu
 *   when it changed: a form refused field by field comes back as its page (422), with the
 *   errors on its fields. `X-OpenShop-Location` sends the page elsewhere; `X-OpenShop-Toast` and
 *   `X-OpenShop-Toast-Error` are shown with `shopify.toast.show`.
 *
 * - What only a browser knows: a `<time datetime>` is written in its language and time zone; a
 *   modal with `data-openshop-open` is opened (its form was refused); an element with
 *   `data-openshop-refresh="3000"` has the page fetched again after that many milliseconds (a run
 *   in progress), unless a field of the page has the focus or was changed, or a modal is open; a
 *   button with `data-openshop-copy` copies that text, or with `data-openshop-copy-from="<id>"`
 *   the text of that element; one with `data-openshop-download="/api/…"` saves what that URL
 *   answers, or with `data-openshop-download-from="<id>"` the text of that element, under the
 *   name of its `data-openshop-filename`.
 *
 * Every request carries `X-OpenShop-Fetch: 1`: the server answers it this way. Without this
 * script, a form posts as usual and the server answers with a redirect.
 */
'use strict'
;(() => {
  if (window.openshop) return
  const ROOT = 'openshop-page'
  const GENERIC_ERROR = 'Something went wrong. Try again.'
  const LINKS = 's-link[href], a[href], s-button[href], s-clickable[href]'
  // Paths of the server that are not pages.
  const SERVER = /^\/(api|auth|webhooks|proxy|ext|routes|health|ops|__openshop)(\/|$)/
  let current = null // the request in flight: a newer one replaces it
  let refresh = null // the timer of `data-openshop-refresh`
  const shown = new Set() // the modals open: a refresh would close them
  let changed = false // a field of the page was changed: a refresh would lose it
  // The Polaris fields whose Enter submits their form, as a native `<input>` does.
  const FIELDS = /^s-(text|search|number|email|url|password|money|date|color)-field$/

  const app = () => window.shopify
  const attempt = (action) => {
    try {
      return action()
    } catch {
      return undefined
    }
  }
  const toast = (message, isError = false) => attempt(() => app().toast.show(message, { isError }))
  const loading = (on) => attempt(() => app().loading(on))
  const decoded = (response, name) => {
    const value = response.headers.get(name)
    return value === null ? null : attempt(() => decodeURIComponent(value)) ?? value
  }
  // A URL of a page of the app, or null for anything the browser should follow itself.
  function pageUrl(href) {
    const url = attempt(() => new URL(href, location.href))
    return url && url.origin === location.origin && !SERVER.test(url.pathname) ? url : null
  }

  // The page the server rendered, in place of the one shown.
  function present(html, url, push) {
    const next = new DOMParser().parseFromString(html, 'text/html')
    const root = next.getElementById(ROOT)
    if (!root) return location.assign(url)
    document.title = next.title
    const menu = next.querySelector('ui-nav-menu')
    const shown = document.querySelector('ui-nav-menu')
    if (menu && shown && menu.outerHTML !== shown.outerHTML) shown.replaceWith(document.importNode(menu, true))
    const nodes = [...root.childNodes].map((node) => document.importNode(node, true))
    document.getElementById(ROOT).replaceChildren(...nodes)
    if (push) history.pushState(null, '', url)
    else if (url !== location.href) history.replaceState(null, '', url)
    changed = false
    ready()
  }

  // What the page asks of the browser, once it is shown.
  function ready() {
    const root = document.getElementById(ROOT)
    if (!root) return
    for (const time of root.querySelectorAll('time[datetime]')) {
      const date = new Date(time.getAttribute('datetime'))
      if (!Number.isNaN(date.getTime())) time.textContent = date.toLocaleString()
    }
    for (const modal of root.querySelectorAll('[data-openshop-open]')) {
      customElements.whenDefined(modal.localName).then(() => attempt(() => modal.showOverlay()))
    }
    for (const modal of shown) if (!modal.isConnected) shown.delete(modal)
    clearTimeout(refresh)
    const every = Number(root.querySelector('[data-openshop-refresh]')?.getAttribute('data-openshop-refresh'))
    if (every > 0) refresh = setTimeout(function again() {
      const focused = document.activeElement !== document.body && root.contains(document.activeElement)
      const busy = current || focused || changed || [...shown].some((modal) => modal.isConnected)
      if (busy) refresh = setTimeout(again, every)
      else visit(location.href, { push: false, quiet: true })
    }, every)
  }

  async function copy(text) {
    try {
      await navigator.clipboard.writeText(text)
      toast('Copied')
    } catch {
      toast('Could not copy', true)
    }
  }

  function save(blob, name) {
    const link = Object.assign(document.createElement('a'), { download: name })
    link.href = URL.createObjectURL(blob)
    link.click()
    setTimeout(() => URL.revokeObjectURL(link.href), 0)
  }

  async function download(url, name) {
    loading(true)
    try {
      const response = await fetch(url, { headers: { 'X-OpenShop-Fetch': '1' } })
      if (!response.ok) return toast(decoded(response, 'X-OpenShop-Toast-Error') || GENERIC_ERROR, true)
      save(await response.blob(), name)
    } catch {
      toast(GENERIC_ERROR, true)
    } finally {
      loading(false)
    }
  }

  // The text of the element `id`, for a copy or a download that names it.
  const textOf = (id) => document.getElementById(id)?.textContent ?? 

  async function visit(url, { method = 'GET', body, push = true, button, quiet = false } = {}) {
    current?.abort()
    const request = (current = new AbortController())
    button?.setAttribute('loading', '')
    if (!quiet) loading(true)
    try {
      const headers = { 'X-OpenShop-Fetch': '1', Accept: 'text/html' }
      const response = await fetch(url, { method, body, headers, signal: request.signal })
      const message = decoded(response, 'X-OpenShop-Toast')
      const failure = decoded(response, 'X-OpenShop-Toast-Error')
      if (message) toast(message)
      if (failure) toast(failure, true)
      const location_ = response.headers.get('X-OpenShop-Location')
      if (location_ && pageUrl(location_)) return visit(pageUrl(location_).href)
      // A page, also one that is not `ok`: a form refused field by field (422), a page that is not there.
      const html = response.headers.get('Content-Type')?.includes('text/html')
      if (html) present(await response.text(), method === 'GET' ? response.url : location.href, push)
      else if (!failure) toast(GENERIC_ERROR, true)
    } catch (error) {
      if (error.name !== 'AbortError' && !quiet) toast(GENERIC_ERROR, true)
    } finally {
      button?.removeAttribute('loading')
      if (current === request) {
        current = null
        if (!quiet) loading(false)
      }
    }
  }

  document.addEventListener('submit', (event) => {
    const form = event.target
    if (event.defaultPrevented || !(form instanceof HTMLFormElement) || form.hasAttribute('data-openshop-native')) return
    const url = pageUrl(form.getAttribute('action') || location.href)
    if (!url) return
    event.preventDefault()
    const submitter = event.submitter
    const data = attempt(() => new FormData(form, submitter)) ?? new FormData(form)
    const button = submitter?.closest('s-button') || form.querySelector('s-button[type="submit"]')
    if ((form.getAttribute('method') || 'get').toLowerCase() === 'get') {
      url.search = new URLSearchParams(data).toString()
      return visit(url.href, { button })
    }
    visit(url.href, { method: 'POST', body: new URLSearchParams(data), push: false, button })
  })

  function follow(event, link) {
    const target = (link.getAttribute('target') || '_self').toLowerCase()
    const url = target === '_self' && pageUrl(link.getAttribute('href'))
    if (!url || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || link.hasAttribute('download')) return
    event.preventDefault()
    visit(url.href)
  }

  // Polaris announces the links of its components; App Bridge clicks the `<a>` of the menu.
  document.addEventListener('shopify:navigate', (event) => {
    const link = event.composedPath().find((node) => node instanceof Element && node.matches(LINKS))
    if (link && !event.defaultPrevented) follow(event, link)
  })
  document.addEventListener('click', (event) => {
    const target = event.target instanceof Element ? event.target : null
    const copied = target?.closest('[data-openshop-copy], [data-openshop-copy-from]')
    if (copied) {
      const from = copied.getAttribute('data-openshop-copy-from')
      return copy(from === null ? copied.getAttribute('data-openshop-copy') : textOf(from))
    }
    const saved = target?.closest('[data-openshop-download], [data-openshop-download-from]')
    if (saved) {
      const name = saved.getAttribute('data-openshop-filename') || 'download'
      const from = saved.getAttribute('data-openshop-download-from')
      if (from === null) return download(saved.getAttribute('data-openshop-download'), name)
      return save(new Blob([textOf(from)], { type: 'application/json' }), name)
    }
    const link = target?.closest('a[href]')
    if (link && !event.defaultPrevented && event.button === 0) follow(event, link)
  })
  addEventListener('popstate', () => visit(location.href, { push: false }))
  // A change: a `GET` form that asks for it is sent; otherwise a refresh waits.
  const edited = (event) => {
    const form = event.target instanceof Element && event.target.closest('form')
    if (event.type === 'change' && form?.hasAttribute('data-openshop-autosubmit')) return form.requestSubmit()
    if (document.getElementById(ROOT)?.contains(event.target)) changed = true
  }
  document.addEventListener('input', edited, true)
  document.addEventListener('change', edited, true)
  document.addEventListener('keydown', (event) => {
    const field = event.target instanceof Element ? event.target : null
    const form = field && FIELDS.test(field.localName) && field.closest('form')
    if (event.key !== 'Enter' || !form || event.isComposing || event.defaultPrevented) return
    event.preventDefault()
    form.requestSubmit()
  })
  // Polaris says when a modal opens and closes; the events do not bubble, a capture sees them.
  document.addEventListener('show', (event) => shown.add(event.target), true)
  document.addEventListener('afterhide', (event) => shown.delete(event.target), true)
  ready()

  window.openshop = { visit, toast }
})()
