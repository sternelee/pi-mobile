## Default Permission

Default permissions for pi-native (device capabilities implemented in-tree).

These commands are invoked from the Rust host, not from the webview, so the
ACL is not the authorization boundary here — capability-level consent is
handled by the OS permission dialogs plus pi-mobile's approval policy
(see src-tauri/src/native/mod.rs).

#### This default permission set includes the following:

- `allow-location`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`pi-native:allow-calendar`

</td>
<td>

Enables the calendar command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`pi-native:deny-calendar`

</td>
<td>

Denies the calendar command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`pi-native:allow-location`

</td>
<td>

Enables the location command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`pi-native:deny-location`

</td>
<td>

Denies the location command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`pi-native:allow-requestPermission`

</td>
<td>

Enables the requestPermission command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`pi-native:deny-requestPermission`

</td>
<td>

Denies the requestPermission command without any pre-configured scope.

</td>
</tr>
</table>
