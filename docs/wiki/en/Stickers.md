# Sticker panel

[简体中文](../zh-CN/Stickers.md) · [Guide](Home.md)

Press Ctrl-T in the composer to open the sticker panel. The panel lists your
Recent and Favorites stickers followed by every installed sticker set, and
sends the selected sticker as a native Telegram sticker — no re-upload, and
your draft text stays untouched.

| In the sticker panel | Action |
| --- | --- |
| hjkl or arrow keys | Move in the sticker grid |
| PageUp/PageDown, Home/End | Move through the stickers |
| Tab / Shift-Tab | Next / previous section |
| Mouse click | Select a section or sticker; click the selected sticker again to send |
| Enter or Space | Send the selected sticker |
| Ctrl-R | Revalidate Recent, Favorites and the set list; retry a failed set |
| Esc or q | Close and return to the composer |

The left sidebar lists the sections: Recent, Favorites and one row per
installed set. The panel opens instantly from the local cache — already
available offline for browsing — and revalidates with Telegram in the
background, showing a subtle refreshing indication until the answer arrives.
Unchanged sections cost no network payload: Telegram's hash mechanism reports
them as not modified. Set contents load lazily when a section is first opened,
with the same cached-then-revalidated behavior. A failed revalidation keeps
the cached data and shows the error in the footer; Ctrl-R forces another pass.

Grid cells render each sticker's static raster thumbnail with the same inline
image pipeline as message previews; cells whose thumbnail has not downloaded
yet show the sticker's emoji. Animated TGS/WebM stickers use their static
thumbnail, which is expected. With no cache and lists still loading, the panel
shows a loading notice; empty sections say so. Thumbnails download lazily, at
most four transfers in flight, and persist in the media cache per document.

Sending honors the current reply: the sticker attaches the reply target and
clears the reply state, exactly like sending the draft, but the draft text is
preserved. An optimistic pending message appears immediately and is replaced
when Telegram confirms; a failed send marks only that message. Sticker file
references expire after some days, so when Telegram reports an expired one,
Termgram refreshes the sticker's set once and retries the send automatically.
Send restrictions and slow mode are enforced like for text.

The entry action is `stickers` in the `compose` context (default Ctrl-T).
Panel keys use the `stickers` context with the actions `open`, `cancel`,
`refresh`, `up`, `down`, `left`, `right`, `page_up`, `page_down`, `home`,
`end`, `sticker_set_next` and `sticker_set_previous`.
