/*
 * openshop.js: the script of the admin pages the server renders. Written by hand, embedded in the
 * binary, no dependency and no build. The server does the work; this script only presents it.
 *
 * - A form is sent in the background with App Bridge's fetch, which adds the session token. Its
 *   submit `s-button` has `loading` while the request runs. A form with `data-openshop-native` is
 *   left to the browser.
 * - A link of the app (`<a>`, and the `s-link` and `s-button` Polaris announces with
 *   `shopify:navigate`) is fetched the same way, and back and forward too.
 * - What the server answers replaces the content of `#openshop-page`, the title, and the menu
 *   when it changed. `X-OpenShop-Location` sends the page elsewhere; `X-OpenShop-Toast` and
 *   `X-OpenShop-Toast-Error` are shown with `shopify.toast.show`.
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
  }

  async function visit(url, { method = 'GET', body, push = true, button } = {}) {
    current?.abort()
    const request = (current = new AbortController())
    button?.setAttribute('loading', '')
    loading(true)
    try {
      const headers = { 'X-OpenShop-Fetch': '1', Accept: 'text/html' }
      const response = await fetch(url, { method, body, headers, signal: request.signal })
      const message = decoded(response, 'X-OpenShop-Toast')
      const failure = decoded(response, 'X-OpenShop-Toast-Error')
      if (message) toast(message)
      if (failure) toast(failure, true)
      const location_ = response.headers.get('X-OpenShop-Location')
      if (location_ && pageUrl(location_)) return visit(pageUrl(location_).href)
      const html = response.headers.get('Content-Type')?.includes('text/html')
      if (response.ok && html) present(await response.text(), method === 'GET' ? response.url : location.href, push)
      else if (!failure) toast(GENERIC_ERROR, true)
    } catch (error) {
      if (error.name !== 'AbortError') toast(GENERIC_ERROR, true)
    } finally {
      button?.removeAttribute('loading')
      if (current === request) {
        current = null
        loading(false)
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
    const link = event.target instanceof Element && event.target.closest('a[href]')
    if (link && !event.defaultPrevented && event.button === 0) follow(event, link)
  })
  addEventListener('popstate', () => visit(location.href, { push: false }))

  window.openshop = { visit, toast }
})()
